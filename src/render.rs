//! Renderer core — owns the wgpu `Device`, `Queue`, `Surface`, the
//! pipelines, the camera, and the tile cache.
//!
//! ## Architecture
//!
//! Each frame the renderer:
//! 1. **Drains** any tile-fetch completions that arrived since the last
//!    frame and uploads the bytes to GPU textures.
//! 2. **Ensures** the currently-visible tile set has been requested
//!    (spawning background fetches for anything not already loaded or
//!    in flight).
//! 3. **Draws** the Earth texture sphere (bundled Blue Marble PNG),
//!    then every visible loaded tile on top, then the polar caps, and
//!    finally the vector overlay. The Earth texture covers anywhere
//!    tiles haven't loaded yet — globe view, cold cache, polar
//!    latitudes outside the Mercator pyramid, the back hemisphere.
//!
//! ## Threading
//!
//! Native fetches spawn a `std::thread` per request and post their
//! result back via `std::sync::mpsc`. Web fetches use
//! `wasm_bindgen_futures::spawn_local` — single-threaded but
//! `mpsc::Sender` still works as the in-process handoff.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::body::{self, Basemap, BasemapId, Body, BodyId};
use crate::camera::Camera;
use crate::clock::SimClock;
use crate::orbit::{self, Category, Satellite};
use crate::tile::{self, DecodedTile, TileId, TileProjection};

/// Map a `TileProjection` to the integer encoding the tile shader's
/// `projection_kind` uniform expects (must agree with the
/// `if (projection == 0u)` branch in `tile.wgsl`).
fn projection_to_u32(p: TileProjection) -> u32 {
    match p {
        TileProjection::WebMercator => 0,
        TileProjection::Equirectangular => 1,
    }
}
use crate::vector::{self, VectorLayer};

const TILE_SHADER: &str = include_str!("shaders/tile.wgsl");
const VECTOR_SHADER: &str = include_str!("shaders/vector.wgsl");
const CAPS_SHADER: &str = include_str!("shaders/caps.wgsl");
const EARTH_SHADER: &str = include_str!("shaders/earth.wgsl");
const ORBIT_SHADER: &str = include_str!("shaders/orbit.wgsl");
const ORBIT_TRAIL_SHADER: &str = include_str!("shaders/orbit_trail.wgsl");
const ATMOSPHERE_SHADER: &str = include_str!("shaders/atmosphere.wgsl");
const STARFIELD_SHADER: &str = include_str!("shaders/starfield.wgsl");
const BUILDING_SHADER: &str = include_str!("shaders/building.wgsl");

/// Frames the camera state must stay unchanged before we consider it
/// "settled" and trigger a satellite-tile fetch. At 60 fps this is
/// ~0.5 s — long enough that mid-pan / mid-zoom intent isn't acted
/// on, short enough that a deliberate pause feels responsive.
const SAT_DWELL_FRAMES: u32 = 30;

/// How many times a satellite-tile fetch is allowed to try before we
/// give up and mark it permanently failed for this camera position.
/// The first attempt is "1," so this number includes the original
/// dispatch — `5` means original + 4 retries. Esri's CDN sometimes
/// serves a header-stripped response from a misconfigured edge; a
/// single retry usually lands on a working edge, but under fetch
/// pressure (browsers throttle ~6 concurrent requests per origin)
/// some edges trip "Failed to fetch" and need more retries to land.
const SAT_MAX_ATTEMPTS: u32 = 5;

/// Maximum concurrent in-flight satellite-tile fetches. Browsers
/// limit concurrent connections per origin (~6 in Chrome/Firefox);
/// dispatching well past that gets some requests stuck or rejected
/// outright with "Failed to fetch". Keep the in-flight count
/// comfortably under the browser's per-origin cap so each request
/// gets a working socket and our retries land cleanly.
const SAT_MAX_INFLIGHT: usize = 6;

/// Maximum new satellite-tile fetches dispatched per call to
/// `dispatch_visible_sat_tiles`. Combined with the per-frame call
/// from the post-dwell loop, this caps the dispatch rate to
/// `MAX_PER_FRAME × 60 fps = 480 tiles/sec`, well within tile CDN
/// budgets while still draining a 130-tile sphere-cap selection
/// in under three seconds.
const SAT_MAX_DISPATCH_PER_FRAME: usize = 8;

// The bundled Blue Marble JPEG used to live at
// `EARTH_JPG_BYTES = include_bytes!("../data/blue-marble/...")`
// here; in plan 0003 M2 it became `body::EARTH.fallback_texture`
// since every body now has a per-body fallback. The WebGPU
// `max_texture_dimension_2d` raise from 2048 → 4096 in
// `request_device` still covers the 4096×2048 Blue Marble texture.

/// Vertex count for the full Earth sphere — `LAT_BANDS × LON_SEGMENTS`
/// quads × 6 verts/quad. Mirrors the constants in `earth.wgsl`.
const EARTH_DRAW_VERTS: u32 = 64 * 128 * 6;

/// Triangles per polar cap. Matches `RING_VERTS` in `caps.wgsl`.
const CAP_RING_VERTS: u32 = 64;
/// Vertex count per cap = 3 verts × `CAP_RING_VERTS` triangles.
const CAP_DRAW_VERTS: u32 = 3 * CAP_RING_VERTS;

/// Atmosphere-shell vertex count — mirrors the `LAT_BANDS *
/// LON_SEGMENTS * QUAD_VERTS` constants at the top of
/// `atmosphere.wgsl`. Keep in lock-step with that file.
const ATMOSPHERE_DRAW_VERTS: u32 = 48 * 96 * 6;

const TILE_UNIFORM_SIZE: u64 = std::mem::size_of::<TileUniforms>() as u64;

/// Construct a wgpu instance suitable for both native and browser targets.
pub fn make_instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    })
}

/// Per-tile uniform consumed by `tile.wgsl`. Matches the WGSL
/// `Uniforms` struct byte-for-byte (8 × `vec4` = 128 bytes after
/// the day/night extension):
/// - rows 0–3: view-projection matrix (column-major)
/// - row 4: camera position (xyz) + per-frame `tile_alpha` (the
///   smoothstepped zoom-fade multiplier)
/// - row 5: tile's world rect (xmin, ymin, xmax, ymax)
/// - row 6: projection kind (u32) + 3 pad u32
/// - row 7: sun direction (xyz) + per-body `night_dim` scalar
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct TileUniforms {
    view_proj: [f32; 16],
    camera_pos: [f32; 3],
    tile_alpha: f32,
    world_rect: [f32; 4],
    /// 0 = WebMercator, 1 = Equirectangular. The shader's
    /// `world_to_lonlat_rad` inverse differs across projections.
    /// Plan 0003 M1.
    projection_kind: u32,
    _pad: [u32; 3],
    /// Sun direction in body-fixed frame (prime meridian at +Z).
    /// Computed each frame from `SimClock` via `sun::direction_from_unix`.
    /// Plan 0009 M0.
    sun_dir: [f32; 3],
    /// Per-body night-side dim multiplier. The fragment uses
    /// `mix(night_dim, 1.0, smoothstep(0.0, 0.15, dot(sphere, sun_dir)))`
    /// to fade between day and night intensities.
    night_dim: f32,
}

/// Per-frame camera uniform consumed by `vector.wgsl`. Matches the
/// WGSL `Camera` struct byte-for-byte (7 × `vec4` = 112 bytes after
/// the day/night extension):
/// - rows 0–3: view-projection matrix (column-major)
/// - row 4: camera position (xyz) + 1 pad
/// - row 5: color (rgba)
/// - row 6: sun direction (xyz) + night_dim
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct VectorCameraUniform {
    view_proj: [f32; 16],
    position: [f32; 3],
    _pad0: f32,
    color: [f32; 4],
    sun_dir: [f32; 3],
    night_dim: f32,
}

/// Per-cap uniform consumed by `caps.wgsl`. Mirrors the layout of
/// `VectorCameraUniform` but with the colour slot reused as the
/// cap's solid colour and a sibling `pole_sign` (+1 north, −1 south)
/// in the camera_pos pad. 96 bytes after the day/night extension.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct CapUniform {
    view_proj: [f32; 16],
    camera_pos: [f32; 3],
    pole_sign: f32,
    color: [f32; 4],
    sun_dir: [f32; 3],
    night_dim: f32,
}

/// Per-frame camera uniform consumed by `orbit.wgsl`. 96 bytes
/// (64 view_proj + 12 cam_pos + 4 pad + 8 viewport + 8 pad).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct OrbitCameraUniform {
    view_proj: [f32; 16],
    camera_pos: [f32; 3],
    _pad0: f32,
    viewport_px: [f32; 2],
    _pad1: [f32; 2],
}

/// Per-instance data for `orbit.wgsl`. 24 bytes — vec3 position +
/// vec3 colour + per-instance `highlight` factor. Position lives in
/// renderer body-fixed coords (Earth radii); colour is linear-light
/// RGB (the CPU side converts sRGB8 → linear via `srgb8_to_linear`
/// before upload). `highlight` is 0.0 for normal satellites and 1.0
/// for the satellite under the cursor in the list — the shader
/// scales up + brightens it so the user sees which dot they're
/// hovering.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct OrbitInstance {
    world_pos: [f32; 3],
    color: [f32; 3],
    highlight: f32,
}

/// Camera uniform for `orbit_trail.wgsl`. 80 bytes. Per-trail
/// colour now lives in the vertex attributes (one bright trail
/// for the selected satellite, faint trails for other satellites
/// in small categories — see `tick_orbit`).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct OrbitTrailUniform {
    view_proj: [f32; 16],
    camera_pos: [f32; 3],
    _pad0: f32,
}

/// Per-vertex data for `orbit_trail.wgsl`. Position in render-space
/// + linear-light RGBA. LineList pairs of these form one segment.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct OrbitTrailVertex {
    position: [f32; 3],
    color: [f32; 4],
}

/// Per-frame camera uniform consumed by `earth.wgsl`. 96 bytes
/// after the day/night extension — view_proj, camera_pos + pad,
/// sun direction + night_dim.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct EarthCameraUniform {
    view_proj: [f32; 16],
    position: [f32; 3],
    _pad0: f32,
    sun_dir: [f32; 3],
    night_dim: f32,
}

/// Per-frame uniform consumed by `building.wgsl`. 128 bytes
/// (8 × vec4 rows). Mirrors the WGSL struct of the same name
/// byte-for-byte. Plan 0014 M1.
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

/// Per-frame uniform consumed by `starfield.wgsl`. 48 bytes —
/// camera pos + aspect ratio, up-hint + zoom-driven strength, sun
/// direction (for the sun-glyph disc + halo) + trailing pad.
/// Mirrors the WGSL struct of the same name byte-for-byte.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct StarfieldUniform {
    camera_pos: [f32; 3],
    aspect: f32,
    up_hint: [f32; 3],
    strength: f32,
    sun_dir: [f32; 3],
    _pad: f32,
}

/// Per-frame uniform consumed by `atmosphere.wgsl`. 144 bytes
/// (9 × vec4). Owns the per-body Rayleigh + Mie params alongside
/// the camera / sun state; one buffer is rewritten each frame for
/// the active body's atmosphere when present (plan 0008 M1+M2).
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct AtmosphereUniform {
    view_proj: [f32; 16],
    camera_pos: [f32; 3],
    planet_radius: f32,
    sun_dir: [f32; 3],
    atmosphere_radius: f32,
    rayleigh_beta: [f32; 3],
    sun_intensity: f32,
    mie_beta: [f32; 3],
    mie_g: f32,
    rayleigh_scale: f32,
    mie_scale: f32,
    /// Zoom-driven 0..1 fade (full at globe view, 0 at street zoom).
    /// Matches the window used by `day_night_color` in the surface
    /// shaders so atmosphere + terminator appear / disappear in
    /// lockstep.
    strength: f32,
    _pad: f32,
}

/// Which basemap the user is currently looking at. Mutually exclusive
/// — the two are alternative views of the same Earth, not layers, so
/// switching turns the other off both at draw time and at fetch time
/// (a Satellite session shouldn't burn Carto requests on a hidden
/// pyramid, and vice versa).
///
/// Compatibility shim: this enum used to be the renderer's basemap-
/// mode field. After plan 0003 M0 the renderer holds a
/// `(BodyId, BasemapId)` pair instead. `BasemapMode` is kept as a
/// public surface only for the wasm-bindgen entry points
/// (`set_basemap("map" | "satellite")`) that pre-date multi-body.
/// The web UI converts between the slug string and this enum at
/// the boundary.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum BasemapMode {
    /// Carto Voyager street basemap.
    Map,
    /// Esri World Imagery, streamed lazily on camera dwell over a
    /// bundled Blue Marble fallback. Default: the satellite view is
    /// the headline experience and lands the user straight into it.
    #[default]
    Satellite,
}

impl BasemapMode {
    /// Convert to the body's `BasemapId` slug.
    pub fn to_basemap_id(self) -> BasemapId {
        match self {
            BasemapMode::Map => BasemapId("map"),
            BasemapMode::Satellite => BasemapId("satellite"),
        }
    }

    /// Map a body's `BasemapId` back to the enum, defaulting to
    /// Satellite for unknown slugs (Mars / Moon basemap slugs).
    pub fn from_basemap_id(id: BasemapId) -> BasemapMode {
        match id.0 {
            "map" => BasemapMode::Map,
            _ => BasemapMode::Satellite,
        }
    }
}

/// A vector-layer that's been uploaded as a GPU vertex buffer, ready
/// to render as a LineList.
struct VectorBinding {
    vertex_buf: wgpu::Buffer,
    vertex_count: u32,
}

/// GPU resources for a loaded city's building mesh — single VBO +
/// IBO + per-building storage buffer + the bind group binding the
/// storage buffer to the building pipeline at binding 1.
struct BuildingBinding {
    vbo: wgpu::Buffer,
    ibo: wgpu::Buffer,
    /// Held so the storage buffer's view inside `bind_group`
    /// stays alive for the binding's lifetime.
    _per_building_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Number of indices in `ibo` — drives `pass.draw_indexed`.
    index_count: u32,
    /// Tallest building's normalised-units height. Passed to
    /// `view_projection_matrix_with_floor` so the near plane
    /// doesn't clip the loaded city's tallest tower.
    max_height_world: f32,
}

/// A raster tile that's been uploaded to the GPU.
struct TileBinding {
    /// Kept alive so the bind-group's texture view stays valid.
    _texture: wgpu::Texture,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// Fetch completion delivered to the renderer's tile-load channel.
type TileFetchResult = (TileId, Result<DecodedTile, String>);

/// Snapshot of the camera state used to detect "the user has settled
/// here." We bucket fractional zooms (so a 0.001-zoom drift from
/// floating-point in the projection matrix doesn't keep resetting
/// the counter) and round center coords to the nearest 1e-6° — well
/// under one tile span at every level we stream.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
struct SatDwellSnapshot {
    zoom_thousandths: i32,
    lon_micros: i64,
    lat_micros: i64,
    canvas: (u32, u32),
}

impl SatDwellSnapshot {
    fn from_camera(camera: &Camera, canvas: (u32, u32)) -> SatDwellSnapshot {
        SatDwellSnapshot {
            zoom_thousandths: (camera.zoom * 1000.0).round() as i32,
            lon_micros: (camera.center_lonlat.0 * 1_000_000.0).round() as i64,
            lat_micros: (camera.center_lonlat.1 * 1_000_000.0).round() as i64,
            canvas,
        }
    }
}

/// The renderer's per-window/canvas state. One `Renderer` per surface;
/// keep alive for the lifetime of the surface it owns.
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    /// Format used for the per-frame render-pass attachment view —
    /// the **sRGB variant** of the surface's configured format. The
    /// GPU applies linear → sRGB encoding when writing to a view in
    /// this format, so the canvas stores gamma-encoded bytes even
    /// when the surface's native format is linear (the common case
    /// on WebGPU canvases).
    view_format: wgpu::TextureFormat,

    /// Depth attachment for the main pass. Buildings (plan 0014) use
    /// `Less` + write so they occlude each other correctly; every
    /// other pipeline runs `Always` + no-write so draw order still
    /// rules for them. Recreated by `resize` when the surface size
    /// changes.
    depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,

    tile_pipeline: wgpu::RenderPipeline,
    tile_bgl: wgpu::BindGroupLayout,
    tile_sampler: wgpu::Sampler,

    vector_pipeline: wgpu::RenderPipeline,
    vector_camera_buf: wgpu::Buffer,
    vector_bind_group: wgpu::BindGroup,
    vector: Option<VectorBinding>,
    /// Polygon footprints + names paired with the country outlines.
    /// Populated alongside `vector` by `set_vector_data`; used by
    /// `pick_country_at` to answer click-to-identify queries. Plan
    /// 0007.
    identify_index: vector::IdentifyIndex,
    /// Sibling vector binding used to draw the picked feature's
    /// outline in a highlight colour over the regular orange
    /// outlines. Rebuilt on every selection change. Cleared by
    /// any camera-moving interaction.
    highlight_camera_buf: wgpu::Buffer,
    highlight_bind_group: wgpu::BindGroup,
    selected_feature_idx: Option<usize>,
    selected_vector_buf: Option<wgpu::Buffer>,
    selected_vector_count: u32,

    /// Polar-cap pipeline + one uniform buffer per cap. The shader is
    /// pure-procedural (no vertex buffer), so each draw needs only its
    /// own uniform bind-group to differentiate north (blue ocean) from
    /// south (white ice sheet).
    cap_pipeline: wgpu::RenderPipeline,
    north_cap_buf: wgpu::Buffer,
    north_cap_bind_group: wgpu::BindGroup,
    south_cap_buf: wgpu::Buffer,
    south_cap_bind_group: wgpu::BindGroup,

    /// Shared fallback-texture pipeline + per-body GPU resources.
    /// One pipeline draws an equirectangular sphere from whichever
    /// body's `BodyResources` is bound at draw time. Plan 0003 M2.
    body_pipeline: wgpu::RenderPipeline,
    body_resources: HashMap<BodyId, BodyResources>,

    /// Atmospheric-scattering pipeline + uniform buffer. One
    /// procedural sphere mesh at `body.atmosphere.atmosphere_radius`
    /// drawn after the body texture + tiles + caps, additively
    /// blended. Bodies with `atmosphere: None` (the Moon) skip the
    /// draw entirely. Plan 0008.
    atmosphere_pipeline: wgpu::RenderPipeline,
    atmosphere_uniform_buf: wgpu::Buffer,
    atmosphere_bind_group: wgpu::BindGroup,

    /// Procedural starfield drawn first in the pass so the lit
    /// globe + tiles + atmosphere overwrite it where they cover.
    /// One fullscreen quad, fragment hash → ~1000 stars in the
    /// celestial sphere. Zoom-ramped strength fades to 0 by
    /// street zoom.
    starfield_pipeline: wgpu::RenderPipeline,
    starfield_uniform_buf: wgpu::Buffer,
    starfield_bind_group: wgpu::BindGroup,

    /// Extruded-building pipeline (plan 0014 M1). One indexed
    /// draw per loaded city; the city dataset is `None` until
    /// the renderer's startup hook loads the bundled
    /// `chicago.geojson.gz`. The depth attachment on the main
    /// pass keeps building-vs-building occlusion correct.
    building_pipeline: wgpu::RenderPipeline,
    building_bgl: wgpu::BindGroupLayout,
    building_uniform_buf: wgpu::Buffer,
    /// Loaded city's GPU resources. `None` until `load_buildings`
    /// runs.
    building_binding: Option<BuildingBinding>,
    /// Mirrors `vector::IdentifyIndex` shape so M3's picker can
    /// surface building names. Built alongside the GPU binding;
    /// empty until `load_buildings` runs.
    buildings_identify: vector::IdentifyIndex,

    /// Tiles that have been decoded + uploaded to the GPU.
    tiles: HashMap<TileId, TileBinding>,
    /// Tile IDs with a fetch in flight (de-dupes repeated requests
    /// while the user pans across the same set).
    requested: HashSet<TileId>,
    /// Tile IDs whose fetch failed — keeps us from re-requesting them
    /// every frame for the rest of the session. The previous behaviour
    /// (remove from `requested` on failure) caused a tight retry loop
    /// when the tile host was unreachable, filling the console with
    /// thousands of the same fetch error per second.
    failed: HashSet<TileId>,
    completed_tx: mpsc::Sender<TileFetchResult>,
    completed_rx: mpsc::Receiver<TileFetchResult>,

    /// Satellite (Esri World Imagery) streaming cache. Same Web
    /// Mercator XYZ pyramid as Carto, so we render through the
    /// existing `tile_pipeline` and reuse the same `TileUniforms`
    /// layout — only the URL provider and the per-tile JPEG content
    /// differ. Keeping a separate cache from `tiles` lets the user
    /// toggle Map ↔ Satellite without one mode's bitmaps bleeding
    /// into the other.
    sat_tiles: HashMap<TileId, TileBinding>,
    sat_requested: HashSet<TileId>,
    sat_failed: HashSet<TileId>,
    /// Per-tile attempt count. A tile that fails once gets re-dispatched
    /// up to `SAT_MAX_ATTEMPTS - 1` more times before landing in
    /// `sat_failed`. Esri's CloudFront-fronted basemap CDN occasionally
    /// serves a cached response without the `Access-Control-Allow-Origin`
    /// header from an edge node; the browser blocks that response and
    /// our fetch sees a generic fetch error, but the next attempt almost
    /// always lands on a different edge (or revalidates against origin)
    /// and succeeds.
    sat_attempts: HashMap<TileId, u32>,
    sat_completed_tx: mpsc::Sender<TileFetchResult>,
    sat_completed_rx: mpsc::Receiver<TileFetchResult>,
    /// Dwell-tracking state for the lazy satellite fetch. Snapshot
    /// the camera every frame; after `SAT_DWELL_FRAMES` frames at
    /// the same snapshot, fetch the visible-tile set once. Any
    /// movement resets the counter — the streamed layer never gets
    /// in the way mid-gesture.
    sat_dwell_snapshot: Option<SatDwellSnapshot>,
    sat_dwell_frames: u32,

    /// Which body the renderer is currently showing. v1 always
    /// `BodyId::Earth`; Mars/Moon arrive in plan 0003 M2/M3.
    active_body: BodyId,
    /// Which of the active body's basemaps is being drawn + fetched.
    /// For Earth that's `BasemapId("map")` or `BasemapId("satellite")`.
    /// Toggled by the UI overlay (web) or the `B` key (native);
    /// see [`Self::set_basemap_mode`].
    active_basemap: BasemapId,

    /// Camera state. Public for direct mutation by input handlers
    /// (`renderer.camera.pan(...)`, `renderer.camera.zoom_at(...)`).
    /// Direct mutation also implicitly cancels any in-flight fly-to
    /// via the `pan` / `zoom_at` wrappers below; raw `camera.pan`
    /// from input handlers should go through [`Self::user_pan`] /
    /// [`Self::user_zoom_at`] instead so the cancellation fires.
    pub camera: Camera,

    /// In-flight fly-to animation, if any. [`Self::tick`] samples
    /// from this every frame and applies the result to `camera`.
    /// User input (pan / zoom / basemap toggle) clears it so the
    /// animation never fights the user. Plan 0002 M3.
    flyto: Option<crate::flyto::FlyTo>,
    /// Latched true at startup; the first `tick_fly_to` consumes it
    /// to enqueue the from-space fly-in down to the home zoom.
    /// Distinct from `flyto` so the trigger doesn't depend on a
    /// monotonic clock that isn't available at construction time.
    initial_fly_pending: bool,

    /// Simulation clock — drives satellite-orbit propagation
    /// (plan 0004) and, when plan 0010 ships, the day/night
    /// terminator (plan 0009). Constructed at startup at
    /// real-time rate. Plan 0010's full UI is not yet wired.
    sim_clock: SimClock,
    /// Satellite catalog. `load_satellites` appends here, deduping
    /// against `satellite_norad_ids` so two loads of the same TLE
    /// group don't double-count. The per-frame propagation filters
    /// to `enabled_categories` and rewrites the instance buffer.
    satellites: Vec<Satellite>,
    /// NORAD ids already in `satellites`. Used to dedupe `load_satellites`.
    satellite_norad_ids: HashSet<u32>,
    /// Categories the user has toggled on. Stations is on by
    /// default; the others toggle via `set_category_enabled`.
    enabled_categories: HashSet<Category>,
    /// Render-budget cap. If the total number of enabled satellites
    /// would exceed this, the lowest-priority enabled category is
    /// suppressed at draw time + flagged via
    /// `category_demoted(cat)`. Plan 0004 M2.
    orbit_budget_cap: u32,
    /// Category that's currently being suppressed by the budget
    /// guard, if any. The UI reads this to grey out the pill.
    orbit_demoted_category: Option<Category>,
    /// Plan 0004 M1 GPU resources: one shared pipeline + instance
    /// buffer + camera-uniform buffer for the orbit overlay.
    orbit_pipeline: wgpu::RenderPipeline,
    orbit_camera_buf: wgpu::Buffer,
    orbit_bind_group: wgpu::BindGroup,
    /// Current instance buffer capacity (number of instances it
    /// can hold). Grows on demand inside `tick_orbit`.
    orbit_instance_buf: wgpu::Buffer,
    orbit_instance_capacity: u32,
    /// Live instance count this frame.
    orbit_instance_count: u32,

    /// Plan 0004 M3 trail pipeline + per-frame line-strip vertex
    /// buffer. The trail follows whichever satellite is selected
    /// (by NORAD id); `None` means no trail drawn this frame.
    orbit_trail_pipeline: wgpu::RenderPipeline,
    orbit_trail_uniform_buf: wgpu::Buffer,
    orbit_trail_bind_group: wgpu::BindGroup,
    orbit_trail_vertex_buf: wgpu::Buffer,
    orbit_trail_vertex_capacity: u32,
    orbit_trail_vertex_count: u32,
    /// NORAD id of the currently-selected satellite (the one with
    /// its trail drawn). Auto-populated to the ISS at startup;
    /// plan 0004 M4 wires interactive selection.
    selected_satellite: Option<u32>,
    /// NORAD id of the satellite the user is currently hovering in
    /// the side-panel list — drawn with a bigger, brighter point so
    /// list-row → globe-dot correspondence is visible. Distinct from
    /// `selected_satellite`: hover is transient, selection is sticky.
    hovered_satellite: Option<u32>,
    /// Whether to draw orbital trails at all. Toggle in the side
    /// panel header; default on. When off, no per-satellite trails
    /// or selected-satellite trail draws.
    trails_enabled: bool,
    /// User-controlled country-outline visibility. AND-ed with the
    /// body's `show_political_overlays` so Mars / Moon still hide
    /// the (Earth-shaped) outlines regardless of this flag. Default
    /// true; toggle lives in the bottom-left "Borders" pill.
    borders_visible: bool,
    /// NORAD ids the user has explicitly hidden via the per-row
    /// checkbox in the satellite-list panel. Hidden satellites
    /// skip both the dot draw and any trail draw. The set is
    /// session-scoped — empty on construction; persists across
    /// category toggles so re-enabling Stations doesn't bring
    /// back a satellite the user just hid.
    hidden_satellites: HashSet<u32>,
    /// Cache of `(norad_id, render_space_position)` populated each
    /// frame by `tick_orbit`. `satellite_under_cursor` projects
    /// each cached position through the current `view_proj` for
    /// CPU-side hit-testing — simpler than GPU pick-texture
    /// readback, and at <1 ms for 10k satellites it's fast enough.
    orbit_frame_positions: Vec<(u32, [f32; 3])>,
}

impl Renderer {
    /// Initialise a renderer against the given surface. Camera defaults
    /// to Chicago at zoom 10 — the plan 0001 "first tile" position.
    pub async fn new(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
    ) -> Renderer {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("no compatible GPU adapter");
        log::info!("aegis adapter: {:?}", adapter.get_info());

        // On wasm we previously rode the WebGL2 floor
        // (`max_texture_dimension_2d = 2048`) for the widest device
        // coverage. The 4096-wide Blue Marble texture we ship for
        // globe view needs a higher ceiling — so we lift just that
        // single limit from 2048 → 4096 and inherit everything else
        // from the downlevel defaults. 4096 is the floor for any
        // GPU advertising WebGPU (and a near-universal WebGL2
        // ceiling), so this stays compatible with mobile and older
        // discrete cards alike.
        let required_limits = if cfg!(target_arch = "wasm32") {
            wgpu::Limits {
                max_texture_dimension_2d: 4096,
                ..wgpu::Limits::downlevel_webgl2_defaults()
            }
        } else {
            wgpu::Limits::default()
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("aegis-device"),
                required_features: wgpu::Features::empty(),
                required_limits,
                ..Default::default()
            })
            .await
            .expect("failed to create device");

        let surface_caps = surface.get_capabilities(&adapter);
        // Surface format selection has to thread one needle: **the canvas
        // gets sRGB-encoded on writeout**, regardless of whether the
        // underlying surface format is sRGB-suffixed.
        //
        // On native, wgpu typically advertises both `…Unorm` and
        // `…UnormSrgb` for the same swapchain, and we can configure with
        // the sRGB variant directly. On WebGPU, browsers' canvas
        // configuration only accepts the non-sRGB formats
        // (`bgra8unorm` / `rgba8unorm`) — the sRGB variants aren't valid
        // canvas formats. Configuring the surface with the linear
        // format means our shader output gets stored verbatim, then
        // displayed as raw sRGB bytes — one gamma curve too dark
        // (Earth texture, tiles, caps all visibly dim).
        //
        // The fix is the surface's `view_formats` mechanism: configure
        // with the canvas's native (linear) format but declare the sRGB
        // variant as a permitted view. Each frame we cast the surface
        // texture to its sRGB view; the GPU then applies the linear →
        // sRGB encoding on writeout, so the bytes that land in the
        // canvas are correctly gamma-encoded. The browser displays them
        // directly without further conversion → on-screen pixels match
        // what the shader output as linear-light values.
        let preferred = surface_caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(surface_caps.formats[0]);
        let view_format = preferred.add_srgb_suffix();
        log::info!(
            "aegis surface: configured as {:?}, rendering through {:?}",
            preferred,
            view_format
        );

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: preferred,
            width: width.max(1),
            height: height.max(1),
            present_mode: surface_caps.present_modes[0],
            alpha_mode: surface_caps.alpha_modes[0],
            // Empty if the surface is already sRGB (view_format ==
            // preferred); else permit the sRGB cast.
            view_formats: if view_format != preferred {
                vec![view_format]
            } else {
                vec![]
            },
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        let (depth_texture, depth_view) =
            create_depth_texture(&device, config.width, config.height);
        // Pipelines target the sRGB *view* format, not the configured
        // surface format — that's the format the render-pass attachment
        // will be in each frame.
        let format = view_format;

        let tile_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aegis-tile-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // Visible to BOTH stages — the vertex shader reads
                    // world_rect + camera state; the fragment shader
                    // reads `globeness` for the backface discard guard.
                    // Was VERTEX-only before tessellation; the driver
                    // rejects the pipeline if the fragment binding
                    // visibility doesn't match what the shader reads.
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let tile_pipeline = build_tile_pipeline(&device, format, &tile_bgl);

        let tile_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("aegis-tile-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        // Vector pipeline + per-frame camera uniform. Shared bind
        // group (single uniform buffer) — `set_vector_layer` swaps the
        // vertex buffer, but the camera binding stays put.
        let vector_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aegis-vector-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let vector_camera_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("aegis-vector-camera"),
            contents: bytemuck::bytes_of(&VectorCameraUniform::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let vector_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aegis-vector-bg"),
            layout: &vector_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: vector_camera_buf.as_entire_binding(),
            }],
        });
        let vector_pipeline = build_vector_pipeline(&device, format, &vector_bgl);

        // Highlight pipeline reuses `vector_pipeline` + the same
        // `VectorCameraUniform` layout — only the colour differs.
        // Two sibling buffers + bind groups so both colours can be
        // bound back-to-back in one frame without a per-call
        // rewrite.
        let highlight_camera_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("aegis-highlight-camera"),
            contents: bytemuck::bytes_of(&VectorCameraUniform::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let highlight_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aegis-highlight-bg"),
            layout: &vector_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: highlight_camera_buf.as_entire_binding(),
            }],
        });

        // Polar caps. Both use the same bind-group layout as the
        // vector pipeline (single uniform). Two buffers + two bind
        // groups so a single `render()` can draw north and south
        // without a per-frame uniform rewrite.
        let cap_pipeline = build_cap_pipeline(&device, format, &vector_bgl);
        let (north_cap_buf, north_cap_bind_group) =
            make_cap_binding(&device, &vector_bgl, "aegis-north-cap", 1.0);
        let (south_cap_buf, south_cap_bind_group) =
            make_cap_binding(&device, &vector_bgl, "aegis-south-cap", -1.0);

        // Earth texture sphere. Decode the bundled Blue Marble PNG at
        // startup, upload as an sRGB 2D texture, and build the
        // procedurally-tessellated pipeline + per-frame camera
        // uniform. Decode failure on a baked-in asset means the
        // binary was corrupted, so this panics rather than fails
        // silently.
        // Body fallback-texture pipeline. Shared across every body;
        // each body gets its own bind group via `build_body_resources`.
        let (body_pipeline, body_bgl) = build_body_pipeline(&device, format);
        let (atmosphere_pipeline, atmosphere_uniform_buf, atmosphere_bind_group) =
            build_atmosphere_pipeline(&device, format);
        let (starfield_pipeline, starfield_uniform_buf, starfield_bind_group) =
            build_starfield_pipeline(&device, format);
        let (building_pipeline, building_bgl, building_uniform_buf) =
            build_building_pipeline(&device, format);
        let mut body_resources: HashMap<BodyId, BodyResources> = HashMap::new();
        for body in body::all() {
            body_resources.insert(
                body.id,
                build_body_resources(&device, &queue, body, &body_bgl),
            );
        }

        // Orbit pipeline (plan 0004 M1). Uses its own bgl + shader;
        // an instance buffer carries per-satellite world position +
        // colour. Initial instance capacity is one — grows on
        // demand inside `tick_orbit` as satellites are loaded.
        let (orbit_pipeline, orbit_bgl, orbit_camera_buf, orbit_bind_group, orbit_instance_buf) =
            build_orbit_pipeline(&device, format, 1);
        let _ = orbit_bgl; // kept implicit via the bind_group

        // Orbit trail pipeline (plan 0004 M3). LineStrip topology.
        let (
            orbit_trail_pipeline,
            _orbit_trail_bgl,
            orbit_trail_uniform_buf,
            orbit_trail_bind_group,
            orbit_trail_vertex_buf,
        ) = build_orbit_trail_pipeline(&device, format, 1);

        // Blue Marble streaming pipeline. Bind-group layout is the
        // same shape as the Carto tile pipeline (uniform + texture +
        // sampler), so we reuse `tile_bgl`. Separate pipeline because
        // the WGSL is different — the BM shader does equirectangular
        // projection, not inverse Mercator.
        let (sat_completed_tx, sat_completed_rx) = mpsc::channel();
        let (completed_tx, completed_rx) = mpsc::channel();

        // Initial sim clock at wall-clock now, real-time rate.
        let now_mono_s = {
            #[cfg(target_arch = "wasm32")]
            {
                web_sys::window()
                    .and_then(|w| w.performance())
                    .map_or(0.0, |p| p.now() / 1000.0)
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                // Native: `Instant::now()` is monotonic; we just need
                // a starting reference, not a wall-clock value.
                0.0
            }
        };
        let now_unix_s = {
            #[cfg(target_arch = "wasm32")]
            {
                js_sys::Date::now() / 1000.0
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |d| d.as_secs_f64())
            }
        };
        let sim_clock = SimClock::new(now_unix_s, now_mono_s, 1.0);

        Renderer {
            surface,
            device,
            queue,
            config,
            view_format,
            depth_texture,
            depth_view,
            tile_pipeline,
            tile_bgl,
            tile_sampler,
            vector_pipeline,
            vector_camera_buf,
            vector_bind_group,
            vector: None,
            identify_index: vector::IdentifyIndex::default(),
            highlight_camera_buf,
            highlight_bind_group,
            selected_feature_idx: None,
            selected_vector_buf: None,
            selected_vector_count: 0,
            cap_pipeline,
            north_cap_buf,
            north_cap_bind_group,
            south_cap_buf,
            south_cap_bind_group,
            body_pipeline,
            body_resources,
            atmosphere_pipeline,
            atmosphere_uniform_buf,
            atmosphere_bind_group,
            starfield_pipeline,
            starfield_uniform_buf,
            starfield_bind_group,
            building_pipeline,
            building_bgl,
            building_uniform_buf,
            building_binding: None,
            buildings_identify: vector::IdentifyIndex::default(),
            tiles: HashMap::new(),
            requested: HashSet::new(),
            failed: HashSet::new(),
            completed_tx,
            completed_rx,
            sat_tiles: HashMap::new(),
            sat_requested: HashSet::new(),
            sat_failed: HashSet::new(),
            sat_attempts: HashMap::new(),
            sat_completed_tx,
            sat_completed_rx,
            sat_dwell_snapshot: None,
            sat_dwell_frames: 0,
            // Default body + basemap: Earth, Satellite. Same as the
            // pre-multi-body default the user already shipped. The
            // starting camera lands on the body's HomeView — for
            // Earth that's mid-zoom Chicago (z=11 ≈ city + inner
            // suburbs); the same code path is what body-switching
            // will reuse in M4.
            active_body: BodyId::Earth,
            active_basemap: BasemapMode::Satellite.to_basemap_id(),
            // Initial camera lands in globe view directly above the
            // home longitude/latitude; the first frame triggers a
            // fly-in down to `body.home.zoom` so first-load reads
            // as "zoom in from space" rather than dropping the
            // user straight into a street-level view.
            camera: {
                let h = body::EARTH.home;
                Camera::new(h.lon, h.lat, 1.0)
            },
            initial_fly_pending: true,
            flyto: None,
            sim_clock,
            satellites: Vec::new(),
            satellite_norad_ids: HashSet::new(),
            // Nothing enabled by default — the UI starts with an
            // empty satellite-list panel hidden, just the category
            // pill bar in the top-left. The user opts in.
            enabled_categories: HashSet::new(),
            // Budget: 12 000 satellites of mixed categories at
            // ~500 ns per sgp4::propagate ≈ 6 ms of CPU per frame.
            // Leaves room for the rest of the render budget.
            orbit_budget_cap: 12_000,
            orbit_demoted_category: None,
            orbit_pipeline,
            orbit_camera_buf,
            orbit_bind_group,
            orbit_instance_buf,
            orbit_instance_capacity: 1,
            orbit_instance_count: 0,
            orbit_trail_pipeline,
            orbit_trail_uniform_buf,
            orbit_trail_bind_group,
            orbit_trail_vertex_buf,
            orbit_trail_vertex_capacity: 1,
            orbit_trail_vertex_count: 0,
            selected_satellite: None,
            hovered_satellite: None,
            trails_enabled: true,
            borders_visible: true,
            hidden_satellites: HashSet::new(),
            orbit_frame_positions: Vec::new(),
        }
    }

    /// The body currently being rendered.
    fn active_body_ref(&self) -> &'static Body {
        body::by_id(self.active_body)
    }

    /// The basemap currently being rendered. Always a member of
    /// `self.active_body_ref().basemaps`.
    fn active_basemap_ref(&self) -> &'static Basemap {
        self.active_body_ref().basemap(self.active_basemap)
    }

    /// True if the active body+basemap is the only one served by the
    /// eager `tiles` cache (Earth's Carto Map). Every other basemap
    /// streams via the dwell-gated `sat_tiles` cache.
    fn is_carto_map_mode(&self) -> bool {
        self.active_body == BodyId::Earth && self.active_basemap == BasemapId("map")
    }

    /// User pan — pixel delta from a mouse drag. Cancels any
    /// in-flight fly-to before applying the pan, so the animation
    /// doesn't keep gliding under the user's input.
    pub fn user_pan(&mut self, dx_px: f64, dy_px: f64, canvas_px: (u32, u32)) {
        self.flyto = None;
        self.camera.pan(dx_px, dy_px, canvas_px);
    }

    /// User zoom — wheel delta around a cursor position. Cancels
    /// any in-flight fly-to before applying the zoom.
    pub fn user_zoom_at(&mut self, delta: f64, cursor_px: (f64, f64), canvas_size_px: (u32, u32)) {
        self.flyto = None;
        self.camera.zoom_at(delta, cursor_px, canvas_size_px);
    }

    /// Upload a vector overlay's vertex buffer (LineList layout: each
    /// pair of vertices = one segment). Idempotent — repeated calls
    /// replace the existing binding. Drops the previous vertex buffer
    /// via wgpu's normal Resource lifecycle.
    pub fn set_vector_layer(&mut self, layer: &VectorLayer) {
        let bytes = bytemuck::cast_slice::<[f32; 2], u8>(&layer.vertices);
        let vertex_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("aegis-vector-vbo"),
                contents: bytes,
                usage: wgpu::BufferUsages::VERTEX,
            });
        log::info!(
            "set_vector_layer: uploaded {} segments ({} bytes)",
            layer.segment_count(),
            bytes.len()
        );
        self.vector = Some(VectorBinding {
            vertex_buf,
            vertex_count: layer.vertices.len() as u32,
        });
    }

    /// Install the click-to-identify polygon index alongside the
    /// rendered outlines. Loaders that call `vector::load_geojson`
    /// get both halves of the pair and pass each through its own
    /// setter. Plan 0007.
    pub fn set_identify_index(&mut self, index: vector::IdentifyIndex) {
        log::info!("set_identify_index: {} features", index.features.len());
        self.identify_index = index;
    }

    /// Load a city's bundled building footprints (gzipped GeoJSON).
    /// Replaces any previously loaded city. Plan 0014 M1.
    pub fn load_buildings_gz(&mut self, gz_bytes: &[u8]) {
        let parsed = match crate::buildings::load_buildings_geojson_gz(gz_bytes) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("load_buildings_gz: parse failed: {e}");
                return;
            }
        };
        let (t, l, d) = crate::buildings::height_source_counts(&parsed);
        log::info!(
            "load_buildings_gz: {} buildings ({} tagged, {} levels, {} default)",
            parsed.len(),
            t,
            l,
            d
        );
        let mesh = crate::buildings::build_mesh(&parsed);
        let identify = crate::buildings::build_identify_index(&parsed);

        if mesh.vertices.is_empty() || mesh.indices.is_empty() {
            log::warn!("load_buildings_gz: empty mesh, skipping upload");
            return;
        }

        let vbo = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("aegis-building-vbo"),
                contents: bytemuck::cast_slice(&mesh.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let ibo = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("aegis-building-ibo"),
                contents: bytemuck::cast_slice(&mesh.indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        let per_building_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("aegis-building-per-instance"),
                contents: bytemuck::cast_slice(&mesh.per_building),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aegis-building-bg"),
            layout: &self.building_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.building_uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: per_building_buf.as_entire_binding(),
                },
            ],
        });
        log::info!(
            "load_buildings_gz: uploaded {} verts, {} indices, max_h_world={:.5}",
            mesh.vertices.len(),
            mesh.indices.len(),
            mesh.max_height_world
        );
        self.building_binding = Some(BuildingBinding {
            vbo,
            ibo,
            _per_building_buf: per_building_buf,
            bind_group,
            index_count: mesh.indices.len() as u32,
            max_height_world: mesh.max_height_world,
        });
        self.buildings_identify = identify;
    }

    /// Inverse-project a canvas pixel back to a `(lon, lat)` on
    /// the unit sphere, then look up the country / feature whose
    /// polygons contain that point. Returns `None` if the cursor
    /// ray misses the sphere or no feature covers the hit. Plan
    /// 0007.
    ///
    /// **Zoom gate.** Only fires when `altitude > 0.05` — at
    /// street zoom the user clicks to navigate, not to identify,
    /// and a "United States of America" card popping under every
    /// click is just noise. Matches the same threshold the
    /// day/night + atmosphere + starfield strength ramps use, so
    /// the feature card appears where the "globe-ish view"
    /// affordances are already on.
    pub fn pick_feature_at(&mut self, cursor_x: f64, cursor_y: f64) -> Option<String> {
        let canvas = self.size();
        if canvas.0 == 0 || canvas.1 == 0 {
            return None;
        }
        if self.camera.altitude(canvas) < 0.05 {
            return None;
        }
        let ndc_x = (cursor_x / canvas.0 as f64) * 2.0 - 1.0;
        let ndc_y = 1.0 - (cursor_y / canvas.1 as f64) * 2.0;

        let cam = self.camera.camera_3d_position(canvas);
        let ro = [cam[0] as f64, cam[1] as f64, cam[2] as f64];
        let cam_len = (ro[0] * ro[0] + ro[1] * ro[1] + ro[2] * ro[2]).sqrt();
        // Camera always looks at the origin; forward = −normalize(camera_pos).
        let forward = [-ro[0] / cam_len, -ro[1] / cam_len, -ro[2] / cam_len];
        // Mirror `view_projection_matrix`'s pole-up switch so the
        // basis matches the camera the user is actually looking
        // through.
        let up_hint = if self.camera.center_lonlat.1.abs() > 89.0 {
            [0.0_f64, 0.0, 1.0]
        } else {
            [0.0_f64, 1.0, 0.0]
        };
        let cross_fu = [
            forward[1] * up_hint[2] - forward[2] * up_hint[1],
            forward[2] * up_hint[0] - forward[0] * up_hint[2],
            forward[0] * up_hint[1] - forward[1] * up_hint[0],
        ];
        let cross_len = (cross_fu[0].powi(2) + cross_fu[1].powi(2) + cross_fu[2].powi(2)).sqrt();
        if cross_len < 1e-9 {
            return None;
        }
        let right = [
            cross_fu[0] / cross_len,
            cross_fu[1] / cross_len,
            cross_fu[2] / cross_len,
        ];
        let up = [
            right[1] * forward[2] - right[2] * forward[1],
            right[2] * forward[0] - right[0] * forward[2],
            right[0] * forward[1] - right[1] * forward[0],
        ];

        let aspect = canvas.0 as f64 / canvas.1 as f64;
        let tan_half = (60.0_f64.to_radians() * 0.5).tan();
        let rx = ndc_x * aspect * tan_half;
        let ry = ndc_y * tan_half;
        let rd_un = [
            forward[0] + right[0] * rx + up[0] * ry,
            forward[1] + right[1] * rx + up[1] * ry,
            forward[2] + right[2] * rx + up[2] * ry,
        ];
        let rd_len = (rd_un[0].powi(2) + rd_un[1].powi(2) + rd_un[2].powi(2)).sqrt();
        let rd = [rd_un[0] / rd_len, rd_un[1] / rd_len, rd_un[2] / rd_len];

        // Ray–unit-sphere intersection (sphere at origin).
        let b = ro[0] * rd[0] + ro[1] * rd[1] + ro[2] * rd[2];
        let c = ro[0].powi(2) + ro[1].powi(2) + ro[2].powi(2) - 1.0;
        let disc = b * b - c;
        if disc < 0.0 {
            return None;
        }
        let t = -b - disc.sqrt();
        if t < 0.0 {
            return None;
        }
        let p = [ro[0] + t * rd[0], ro[1] + t * rd[1], ro[2] + t * rd[2]];
        // Sphere → lon/lat with prime meridian at +Z (matches
        // `lonlat_to_sphere` in the shaders).
        let lat = p[1].asin().to_degrees();
        let lon = p[0].atan2(p[2]).to_degrees();

        let pick = self
            .identify_index
            .pick_with_index(lon, lat)
            .map(|(idx, f)| (idx, f.name.clone()));
        if let Some((idx, name)) = pick {
            self.set_selected_feature(Some(idx));
            Some(name)
        } else {
            None
        }
    }

    /// Set (or clear) the click-highlighted feature by index. Rebuilds
    /// the small GPU vertex buffer used by the highlight draw. Called
    /// from `pick_feature_at` on a hit, and from
    /// `clear_selected_feature` on any camera-moving interaction.
    pub fn set_selected_feature(&mut self, idx: Option<usize>) {
        if idx == self.selected_feature_idx {
            return;
        }
        self.selected_feature_idx = idx;
        let Some(i) = idx else {
            self.selected_vector_buf = None;
            self.selected_vector_count = 0;
            return;
        };
        let Some(f) = self.identify_index.features.get(i) else {
            self.selected_vector_buf = None;
            self.selected_vector_count = 0;
            return;
        };

        // Project each ring of each polygon into normalised
        // Mercator world coords (same space the main outline
        // vertices live in) and emit LineList pairs. Polygons in
        // GeoJSON aren't guaranteed to repeat the first vertex at
        // the end of each ring — add a closing segment when
        // needed so the highlight outline is fully closed.
        let mut verts: Vec<[f32; 2]> = Vec::new();
        for poly in &f.polygons {
            for ring in poly {
                if ring.len() < 2 {
                    continue;
                }
                for w in ring.windows(2) {
                    let (wx0, wy0) = crate::crs::lonlat_to_world(w[0][0], w[0][1]);
                    let (wx1, wy1) = crate::crs::lonlat_to_world(w[1][0], w[1][1]);
                    verts.push([wx0 as f32, wy0 as f32]);
                    verts.push([wx1 as f32, wy1 as f32]);
                }
                let first = &ring[0];
                let last = &ring[ring.len() - 1];
                if (first[0] - last[0]).abs() > 1e-9 || (first[1] - last[1]).abs() > 1e-9 {
                    let (wx0, wy0) = crate::crs::lonlat_to_world(last[0], last[1]);
                    let (wx1, wy1) = crate::crs::lonlat_to_world(first[0], first[1]);
                    verts.push([wx0 as f32, wy0 as f32]);
                    verts.push([wx1 as f32, wy1 as f32]);
                }
            }
        }
        let bytes = bytemuck::cast_slice::<[f32; 2], u8>(&verts);
        let buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("aegis-highlight-vbo"),
                contents: bytes,
                usage: wgpu::BufferUsages::VERTEX,
            });
        self.selected_vector_buf = Some(buf);
        self.selected_vector_count = verts.len() as u32;
    }

    /// Drop any highlighted feature. Called from the JS side on
    /// camera-moving interactions so a "United States" highlight
    /// doesn't outline the ocean after a rotation.
    pub fn clear_selected_feature(&mut self) {
        self.set_selected_feature(None);
    }

    /// Surface dimensions in physical pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Reconfigure the surface to a new drawable size.
    pub fn resize(&mut self, width: u32, height: u32) {
        let (w, h) = (width.max(1), height.max(1));
        if w == self.config.width && h == self.config.height {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
        let (depth_texture, depth_view) = create_depth_texture(&self.device, w, h);
        self.depth_texture = depth_texture;
        self.depth_view = depth_view;
    }

    /// Upload an RGBA raster tile and bind it under `id`. Idempotent —
    /// repeated calls with the same id replace the existing binding.
    pub fn upload_tile(&mut self, id: TileId, width: u32, height: u32, rgba: &[u8]) {
        assert_eq!(
            rgba.len(),
            (width * height * 4) as usize,
            "upload_tile {id:?}: rgba.len()={} doesn't match {width}×{height}×4",
            rgba.len()
        );

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aegis-tile-texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let uniform_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("aegis-tile-uniform"),
                contents: bytemuck::bytes_of(&TileUniforms::default()),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aegis-tile-bg"),
            layout: &self.tile_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.tile_sampler),
                },
            ],
        });

        self.requested.remove(&id);
        self.tiles.insert(
            id,
            TileBinding {
                _texture: texture,
                uniform_buf,
                bind_group,
            },
        );
    }

    /// Pump any tile-fetch completions that arrived since the last
    /// frame — uploads successful ones to the GPU; drops failures.
    pub fn drain_completed_fetches(&mut self) {
        loop {
            match self.completed_rx.try_recv() {
                Ok((id, Ok(decoded))) => {
                    self.upload_tile(id, decoded.width, decoded.height, &decoded.rgba);
                }
                Ok((id, Err(e))) => {
                    self.requested.remove(&id);
                    self.failed.insert(id);
                    log::warn!("tile fetch failed for {id:?}: {e}");
                }
                Err(mpsc::TryRecvError::Empty) | Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    /// Ensure every currently-visible tile is either loaded or has a
    /// fetch in flight, **plus** the parent tiles at one zoom level
    /// coarser. Parents are 1/4 the count of current-zoom tiles (each
    /// covers 4 children), so the prefetch cost is small — and it
    /// makes zoom-out instant: by the time you scroll to z-1, those
    /// tiles are already on the GPU.
    pub fn ensure_visible_tiles(&mut self) {
        if !self.is_carto_map_mode() {
            // Carto pyramid is hidden (Earth's Satellite basemap or
            // any non-Earth body). Don't burn requests on tiles we
            // won't draw; the cache stays warm for cheap toggle-back.
            return;
        }
        let canvas = self.size();
        let visible = self.camera.visible_tiles(canvas);
        for id in &visible {
            self.request_if_new(*id);
        }
        // Prefetch parents. HashSet dedupes — each parent at
        // (z-1, x/2, y/2) covers up to four visible children.
        if let Some(first) = visible.first() {
            if first.z > 0 {
                let parents: HashSet<TileId> = visible
                    .iter()
                    .map(|t| TileId {
                        z: t.z - 1,
                        x: t.x / 2,
                        y: t.y / 2,
                    })
                    .collect();
                for id in parents {
                    self.request_if_new(id);
                }
            }
        }
    }

    fn request_if_new(&mut self, id: TileId) {
        if self.tiles.contains_key(&id) || self.requested.contains(&id) || self.failed.contains(&id)
        {
            return;
        }
        self.requested.insert(id);
        self.dispatch_tile_fetch(id);
    }

    /// Native: spawn a thread per tile request that performs the
    /// blocking HTTP fetch + PNG decode and posts the result back on
    /// the completion channel.
    #[cfg(not(target_arch = "wasm32"))]
    fn dispatch_tile_fetch(&self, id: TileId) {
        let tx = self.completed_tx.clone();
        let url = body::format_tile_url(self.active_basemap_ref().url_template, id.z, id.x, id.y);
        std::thread::spawn(move || {
            let result = tile::fetch_tile_blocking(&url);
            let _ = tx.send((id, result));
        });
    }

    /// Web: spawn a JS-event-loop task per tile request. Same
    /// completion-channel handoff (mpsc works fine on wasm — it's
    /// single-threaded but the channel synchronises within the thread).
    #[cfg(target_arch = "wasm32")]
    fn dispatch_tile_fetch(&self, id: TileId) {
        let tx = self.completed_tx.clone();
        let url = body::format_tile_url(self.active_basemap_ref().url_template, id.z, id.x, id.y);
        wasm_bindgen_futures::spawn_local(async move {
            let result = tile::fetch_tile_web(&url).await;
            let _ = tx.send((id, result));
        });
    }

    // -----------------------------------------------------------------
    // Satellite (Esri World Imagery) streaming layer. Parallel cache
    // to the Carto one above, same `TileId` keying because both use
    // the Web Mercator pyramid — only the URL provider and the JPEG
    // payload differ. Renders through the existing `tile_pipeline`,
    // so this section is fetch / cache / dwell plumbing only.
    // -----------------------------------------------------------------

    /// Upload a decoded satellite tile to the GPU and bind it under
    /// `id`. Same shape as [`Self::upload_tile`] but writes into the
    /// `sat_*` cache.
    fn upload_sat_tile(&mut self, id: TileId, width: u32, height: u32, rgba: &[u8]) {
        assert_eq!(
            rgba.len(),
            (width * height * 4) as usize,
            "upload_sat_tile {id:?}: rgba.len()={} doesn't match {width}×{height}×4",
            rgba.len()
        );

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("aegis-sat-tile-texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let uniform_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("aegis-sat-tile-uniform"),
                contents: bytemuck::bytes_of(&TileUniforms::default()),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aegis-sat-tile-bg"),
            layout: &self.tile_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.tile_sampler),
                },
            ],
        });
        self.sat_requested.remove(&id);
        self.sat_tiles.insert(
            id,
            TileBinding {
                _texture: texture,
                uniform_buf,
                bind_group,
            },
        );
    }

    /// Drain any satellite-tile fetch completions and upload them.
    pub fn drain_sat_completed_fetches(&mut self) {
        loop {
            match self.sat_completed_rx.try_recv() {
                Ok((id, Ok(decoded))) => {
                    self.upload_sat_tile(id, decoded.width, decoded.height, &decoded.rgba);
                    self.sat_attempts.remove(&id);
                }
                Ok((id, Err(e))) => {
                    let attempts = self.sat_attempts.entry(id).or_insert(0);
                    *attempts += 1;
                    if *attempts < SAT_MAX_ATTEMPTS {
                        // Keep `sat_requested` set so a concurrent
                        // dispatch from a fresh dwell doesn't double up.
                        log::info!(
                            "sat tile retry {}/{SAT_MAX_ATTEMPTS} for {id:?}: {e}",
                            *attempts
                        );
                        self.dispatch_sat_tile_fetch(id);
                    } else {
                        self.sat_requested.remove(&id);
                        self.sat_failed.insert(id);
                        log::warn!(
                            "sat tile fetch failed after {SAT_MAX_ATTEMPTS} attempts \
                             for {id:?}: {e}"
                        );
                    }
                }
                Err(mpsc::TryRecvError::Empty) | Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    /// Dwell-gated satellite fetch. Call from the frame loop on every
    /// tick. The first `SAT_DWELL_FRAMES` frames at any new camera
    /// position do nothing; once the user has settled we fetch the
    /// visible-tile set once and stop until they move again. Movement
    /// resets the counter so the streamed layer never gets in the way
    /// mid-gesture.
    pub fn ensure_visible_sat_tiles(&mut self) {
        if self.is_carto_map_mode() {
            // Carto Map handles its own dispatch via
            // `ensure_visible_tiles`. Every other basemap (Earth's
            // Satellite, Mars's Color / Terrain, Moon's mosaic) flows
            // through the dwell-gated streaming path here.
            return;
        }
        let canvas = self.size();
        let snapshot = SatDwellSnapshot::from_camera(&self.camera, canvas);
        if Some(snapshot) != self.sat_dwell_snapshot {
            // User has moved — reset the dwell counter, clear the
            // failure blocklist, and zero the retry counter so any
            // tile that hit its attempt cap at the previous position
            // gets a fresh budget here.
            self.sat_dwell_snapshot = Some(snapshot);
            self.sat_dwell_frames = 0;
            self.sat_failed.clear();
            self.sat_attempts.clear();
            return;
        }
        if self.sat_dwell_frames < SAT_DWELL_FRAMES {
            self.sat_dwell_frames += 1;
            return;
        }
        // Post-dwell, dispatch on every frame — the dispatcher's
        // own caps (`SAT_MAX_INFLIGHT`, `SAT_MAX_DISPATCH_PER_FRAME`)
        // keep the rate sane, and re-running each frame lets us
        // drain a large visible set over many frames instead of
        // bursting them all at once. Bursting saturates the
        // browser's per-origin connection pool and surfaces as
        // "Failed to fetch" errors at the JS layer.
        self.dispatch_visible_sat_tiles();
        self.sat_dwell_frames = self.sat_dwell_frames.saturating_add(1);
    }

    /// Enqueue (up to) `SAT_MAX_DISPATCH_PER_FRAME` newly-visible
    /// satellite-tile fetches. Bounded so a single dispatch never
    /// bursts past the browser's per-origin connection cap; the
    /// post-dwell loop re-calls this every frame so a large
    /// visible set drains over many frames instead of all at once.
    /// Also gated by `SAT_MAX_INFLIGHT` so we never push more
    /// concurrent requests at the CDN than it can comfortably
    /// service.
    fn dispatch_visible_sat_tiles(&mut self) {
        let inflight = self.sat_requested.len();
        if inflight >= SAT_MAX_INFLIGHT {
            return;
        }
        let budget = SAT_MAX_INFLIGHT - inflight;
        let per_frame = budget.min(SAT_MAX_DISPATCH_PER_FRAME);
        let canvas = self.size();
        let basemap = self.active_basemap_ref();
        let visible = self
            .camera
            .visible_tiles_capped(canvas, basemap.max_z, basemap.projection);
        let visible_count = visible.len();
        let mut dispatched = 0;
        for id in visible {
            if dispatched >= per_frame {
                break;
            }
            if self.sat_tiles.contains_key(&id)
                || self.sat_requested.contains(&id)
                || self.sat_failed.contains(&id)
            {
                continue;
            }
            self.sat_requested.insert(id);
            self.dispatch_sat_tile_fetch(id);
            dispatched += 1;
        }
        if dispatched > 0 {
            log::info!(
                "sat: dispatched {dispatched}/{visible_count} tiles \
                 (camera zoom={:.2}, in-flight={}, cached={})",
                self.camera.zoom,
                self.sat_requested.len(),
                self.sat_tiles.len(),
            );
        }
    }

    /// The basemap currently being shown. Compatibility surface for
    /// the wasm-bindgen pre-multi-body API; new code should call
    /// `active_basemap_id()` instead.
    pub fn basemap_mode(&self) -> BasemapMode {
        BasemapMode::from_basemap_id(self.active_basemap)
    }

    /// HTML credit string for the currently active body + basemap,
    /// pulled from `Basemap::attribution_html` in `body.rs`. Consumed
    /// by the JS attribution overlay (foundation M4) so the panel
    /// reflects the live source whenever the user switches body or
    /// toggles Map ↔ Satellite.
    pub fn active_basemap_attribution_html(&self) -> &'static str {
        self.active_basemap_ref().attribution_html
    }

    /// Switch the basemap on the active body. Calling with the
    /// current basemap is a no-op. When switching **to** Satellite
    /// we skip the dwell wait and dispatch the visible-tile fetch
    /// immediately — the user just asked for satellite imagery, so
    /// a half-second delay would feel like the toggle didn't work.
    pub fn set_basemap_mode(&mut self, mode: BasemapMode) {
        let next = mode.to_basemap_id();
        if self.active_basemap == next {
            return;
        }
        // Basemap toggle is a user input — cancel any in-flight
        // fly-to so the camera doesn't keep gliding under the new
        // basemap.
        self.flyto = None;
        self.active_basemap = next;
        // Skip dwell when switching to any streaming basemap (Earth
        // Satellite, Mars / Moon / future bodies). Switching to
        // Carto Map is eager regardless.
        if !self.is_carto_map_mode() {
            self.dispatch_visible_sat_tiles();
            let canvas = self.size();
            self.sat_dwell_snapshot = Some(SatDwellSnapshot::from_camera(&self.camera, canvas));
            self.sat_dwell_frames = SAT_DWELL_FRAMES.saturating_add(1);
        }
    }

    /// The currently active basemap's slug — `"map"` / `"satellite"`
    /// for Earth. Public for the web UI + URL-state serialisation.
    pub fn active_basemap_id(&self) -> BasemapId {
        self.active_basemap
    }

    /// The currently active body. Public for the web UI body
    /// switcher.
    pub fn active_body_id(&self) -> BodyId {
        self.active_body
    }

    /// Switch to a different body. Clears the streaming tile cache
    /// (the old body's tiles aren't valid for the new one), sets
    /// the active basemap to the new body's default, cancels any
    /// in-flight fly-to, and snaps the camera to the new body's
    /// `HomeView`. Calling with the current body is a no-op.
    ///
    /// The Carto Earth Map cache (`tiles`) is preserved — it's
    /// Earth-specific and stays warm if you bounce back to Earth.
    pub fn set_body(&mut self, body_id: BodyId) {
        if self.active_body == body_id {
            return;
        }
        let body = body::by_id(body_id);
        self.flyto = None;
        self.active_body = body_id;
        self.active_basemap = body.default_basemap();
        // Streaming cache + per-tile request bookkeeping all become
        // stale across bodies. The Carto Earth-only `tiles` cache
        // stays so an Earth → Mars → Earth round-trip is cheap.
        self.sat_tiles.clear();
        self.sat_requested.clear();
        self.sat_failed.clear();
        self.sat_attempts.clear();
        self.sat_dwell_snapshot = None;
        self.sat_dwell_frames = 0;
        // Snap the camera to the new body's home view. M4 may
        // upgrade this to a fly-to once a body-aware spherical
        // transition is fleshed out.
        self.camera = Camera::new(body.home.lon, body.home.lat, body.home.zoom);
        // If the new body isn't Carto-eligible, kick off an
        // immediate streaming dispatch so the first paint isn't
        // bare fallback texture.
        if !self.is_carto_map_mode() {
            self.dispatch_visible_sat_tiles();
            let canvas = self.size();
            self.sat_dwell_snapshot = Some(SatDwellSnapshot::from_camera(&self.camera, canvas));
            self.sat_dwell_frames = SAT_DWELL_FRAMES.saturating_add(1);
        }
    }

    /// Set the active basemap on the current body directly (without
    /// the `BasemapMode` compatibility shim). Used by the body
    /// switcher when the user picks "Color" / "Terrain" / etc. that
    /// don't map cleanly to `BasemapMode::{Map, Satellite}`.
    pub fn set_basemap_by_id(&mut self, id: BasemapId) {
        if self.active_basemap == id {
            return;
        }
        // Verify the basemap exists on the active body — `Body::basemap`
        // panics on miss, which would be the same effective behaviour
        // but with a noisier error. We log instead.
        let body = self.active_body_ref();
        if !body.basemaps.iter().any(|b| b.id == id) {
            log::warn!(
                "set_basemap_by_id: body {:?} has no basemap '{}'",
                body.id,
                id.0
            );
            return;
        }
        self.flyto = None;
        self.active_basemap = id;
        // Same eagerness as set_basemap_mode for non-Carto basemaps.
        if !self.is_carto_map_mode() {
            self.dispatch_visible_sat_tiles();
            let canvas = self.size();
            self.sat_dwell_snapshot = Some(SatDwellSnapshot::from_camera(&self.camera, canvas));
            self.sat_dwell_frames = SAT_DWELL_FRAMES.saturating_add(1);
        }
    }

    // ---------------------------------------------------------------------
    // Fly-to: smooth camera glide to a search-result location. Plan 0002 M3.
    // ---------------------------------------------------------------------

    /// Start a fly-to from the current camera state to
    /// `(target_lonlat, target_zoom)`. `now` is monotonic time in
    /// seconds — the caller supplies it from `Instant::now()` on
    /// native and `performance.now()` on web. Replaces any
    /// in-flight fly-to.
    pub fn fly_to(&mut self, target_lonlat: (f64, f64), target_zoom: f64, now: f64) {
        self.flyto = Some(crate::flyto::FlyTo::to_target(
            &self.camera,
            target_lonlat,
            target_zoom,
            now,
        ));
    }

    /// Start a fly-to that fits `bbox` (as
    /// `[lon_min, lat_min, lon_max, lat_max]`) into the canvas with
    /// a 10% margin on every side. Targets the bbox centre.
    pub fn fly_to_bbox(&mut self, bbox: [f64; 4], now: f64) {
        let canvas = self.size();
        let zoom = crate::flyto::zoom_to_fit_bbox(bbox, canvas);
        let target = crate::flyto::bbox_center(bbox);
        self.fly_to(target, zoom, now);
    }

    /// Cancel any in-flight fly-to without moving the camera.
    /// Call from user-input handlers (pan, zoom, basemap toggle).
    pub fn cancel_fly_to(&mut self) {
        self.flyto = None;
    }

    /// True iff a fly-to is currently animating.
    pub fn is_flying(&self) -> bool {
        self.flyto.is_some()
    }

    /// Advance the fly-to animation by one frame. `now` is the
    /// same monotonic-seconds value the caller passes to `fly_to`.
    /// Clears the animation when complete. No-op when no fly-to
    /// is active.
    pub fn tick_fly_to(&mut self, now: f64) {
        // First-frame from-space fly-in. We can't construct the
        // FlyTo in `Renderer::new` because the monotonic clock is
        // only available here — `now=0` would race with the same-
        // tick `tick_orbit` that already reads the clock. Triggered
        // exactly once, then `initial_fly_pending` stays false for
        // the rest of the session (body switches construct their
        // own short flies via `set_body`).
        if self.initial_fly_pending {
            self.initial_fly_pending = false;
            let h = body::EARTH.home;
            self.flyto = Some(crate::flyto::FlyTo {
                start_lonlat: (h.lon, h.lat),
                start_zoom: 1.0,
                target_lonlat: (h.lon, h.lat),
                target_zoom: h.zoom,
                started_at: now,
                // Bypass the great-circle-arc-derived duration —
                // a same-lonlat fly would otherwise snap in 0.4s.
                // 2.5s gives the user time to actually see the
                // globe before the dive.
                duration: 2.5,
            });
        }
        let Some(fly) = self.flyto else { return };
        let (lonlat, zoom) = fly.sample(now);
        self.camera.center_lonlat = lonlat;
        self.camera.zoom = zoom.clamp(crate::camera::MIN_ZOOM, crate::camera::MAX_ZOOM);
        if fly.is_done(now) {
            self.flyto = None;
        }
    }

    // ---------------------------------------------------------------------
    // Plan 0004 — Live satellite-orbit overlay
    // ---------------------------------------------------------------------

    /// Append satellites parsed from a TLE blob in `category`,
    /// deduped against the existing catalog by NORAD id. The same
    /// TLE loaded twice produces one entry. The second load updates
    /// the existing entry's category if it changed.
    pub fn load_satellites(&mut self, category: Category, tle_text: &str) {
        let tles = orbit::parse_tles(tle_text);
        let prepared = orbit::satellites_from_tles(&tles, category);
        let mut added = 0;
        let mut updated = 0;
        for sat in prepared {
            if self.satellite_norad_ids.contains(&sat.norad_id) {
                // Already loaded under another category. Update the
                // category in-place so an "is this Starlink" lookup
                // reflects the most recent fetch.
                if let Some(existing) = self
                    .satellites
                    .iter_mut()
                    .find(|s| s.norad_id == sat.norad_id)
                {
                    existing.category = sat.category;
                    updated += 1;
                }
            } else {
                self.satellite_norad_ids.insert(sat.norad_id);
                self.satellites.push(sat);
                added += 1;
            }
        }
        log::info!(
            "orbit: category {:?} → {} added, {} updated; catalog now {}",
            category,
            added,
            updated,
            self.satellites.len()
        );
    }

    /// Toggle a category on or off. Off-categories stop propagating
    /// and drawing immediately; the next `tick_orbit` reflects the
    /// change.
    pub fn set_category_enabled(&mut self, category: Category, enabled: bool) {
        if enabled {
            self.enabled_categories.insert(category);
        } else {
            self.enabled_categories.remove(&category);
        }
    }

    /// Is this category currently rendering?
    pub fn category_enabled(&self, category: Category) -> bool {
        self.enabled_categories.contains(&category)
    }

    /// How many satellites in the catalog belong to this category.
    /// Used by the web fallback path to detect when a category
    /// fetch returned nothing and an `active`-group fallback is
    /// warranted.
    pub fn satellite_count_in(&self, category: Category) -> usize {
        self.satellites
            .iter()
            .filter(|s| s.category == category)
            .count()
    }

    /// If the budget guard is currently suppressing a category,
    /// returns it. UI uses this to grey out the demoted pill.
    pub fn demoted_category(&self) -> Option<Category> {
        self.orbit_demoted_category
    }

    /// Select a satellite by NORAD id (or `None` to clear). The
    /// renderer will draw a trail polyline along its orbit at the
    /// next `tick_orbit`. Plan 0004 M3.
    pub fn set_selected_satellite(&mut self, norad: Option<u32>) {
        self.selected_satellite = norad;
        if norad.is_none() {
            self.orbit_trail_vertex_count = 0;
        }
    }

    /// NORAD id of the currently-selected satellite, if any.
    pub fn selected_satellite(&self) -> Option<u32> {
        self.selected_satellite
    }

    /// Look up a satellite by NORAD id. Used by the M4 hover/click
    /// tooltip to surface name + altitude.
    pub fn satellite_by_norad(&self, norad: u32) -> Option<&Satellite> {
        self.satellites.iter().find(|s| s.norad_id == norad)
    }

    /// Set the hovered satellite — the one whose dot renders
    /// bigger and brighter so list-row → globe-dot correspondence
    /// is visible. Distinct from `set_selected_satellite`: hover
    /// is transient (clears on pointer-leave); selection is sticky
    /// (clicked row gets the bright orbit trail).
    pub fn set_hovered_satellite(&mut self, norad: Option<u32>) {
        self.hovered_satellite = norad;
    }

    /// Whether orbital trails are currently drawn. Toggled via the
    /// satellite-list panel's "Show trails" checkbox.
    pub fn trails_enabled(&self) -> bool {
        self.trails_enabled
    }

    /// Turn orbital trails on or off.
    pub fn set_trails_enabled(&mut self, enabled: bool) {
        self.trails_enabled = enabled;
    }

    /// Whether country outlines are currently drawn. AND-ed with
    /// the active body's `show_political_overlays` — Mars / Moon
    /// hide the outlines regardless.
    pub fn borders_visible(&self) -> bool {
        self.borders_visible
    }

    /// Turn the country-outline vector overlay on or off.
    pub fn set_borders_visible(&mut self, visible: bool) {
        self.borders_visible = visible;
    }

    /// Show or hide a single satellite by NORAD id. Hidden
    /// satellites skip the dot draw and any trail draw — useful
    /// when the user wants to focus on one or two satellites in a
    /// busy category. Persists across category toggles within
    /// the same session.
    pub fn set_satellite_visible(&mut self, norad: u32, visible: bool) {
        if visible {
            self.hidden_satellites.remove(&norad);
        } else {
            self.hidden_satellites.insert(norad);
        }
    }

    /// True iff the satellite is visible. Returns true for
    /// unknown NORAD ids (no hide entry means "render normally").
    pub fn satellite_visible(&self, norad: u32) -> bool {
        !self.hidden_satellites.contains(&norad)
    }

    /// Total number of loaded satellites across all categories.
    pub fn satellite_count(&self) -> usize {
        self.satellites.len()
    }

    /// Iterate the loaded satellite catalog as `(norad, name,
    /// category_slug)` triples. The web UI reads this to populate
    /// the side-panel list. Caller may filter / paginate as
    /// appropriate — at 6 000 Starlink the JS side should virtualise
    /// (or truncate) rather than render every row.
    pub fn satellites_iter(&self) -> impl Iterator<Item = (u32, &str, &'static str)> + '_ {
        self.satellites.iter().map(|s| {
            (
                s.norad_id,
                s.name.as_str(),
                match s.category {
                    Category::Stations => "stations",
                    Category::Starlink => "starlink",
                    Category::Gnss => "gnss",
                    Category::Weather => "weather",
                    Category::Debris => "debris",
                    Category::Other => "other",
                },
            )
        })
    }

    /// CPU-side hit-test: returns the NORAD id of the satellite
    /// drawn nearest to the cursor (within a small pick radius),
    /// or `None`. Cursor coords are in **device pixels** (matching
    /// the canvas backing size). Plan 0004 M4.
    ///
    /// Implementation: project each frame-cached satellite position
    /// through `view_proj`, convert clip → screen pixels, and
    /// pick the closest within a tolerance. Skip satellites
    /// occluded by Earth (same camera-ray test the shader does).
    pub fn satellite_under_cursor(&self, cursor_px: (f64, f64)) -> Option<u32> {
        if self.orbit_frame_positions.is_empty() {
            return None;
        }
        let canvas = self.size();
        let view_proj = self.camera.view_projection_matrix(canvas);
        let camera_pos_3d = self.camera.camera_3d_position(canvas);
        let cam = [
            camera_pos_3d[0] as f64,
            camera_pos_3d[1] as f64,
            camera_pos_3d[2] as f64,
        ];

        // Pick radius in device pixels. Matches the orbit shader's
        // POINT_SIZE_PX (5) with a touch of slack so the user
        // doesn't have to land the cursor pixel-perfectly.
        const PICK_RADIUS_PX: f64 = 10.0;
        let pick_r2 = PICK_RADIUS_PX * PICK_RADIUS_PX;

        let canvas_w = canvas.0 as f64;
        let canvas_h = canvas.1 as f64;

        let mut best: Option<(u32, f64)> = None;
        for &(norad, pos) in &self.orbit_frame_positions {
            // Earth-occlusion test (same logic as orbit.wgsl).
            let p = [pos[0] as f64, pos[1] as f64, pos[2] as f64];
            let seg = [p[0] - cam[0], p[1] - cam[1], p[2] - cam[2]];
            let seg_len2 = seg[0] * seg[0] + seg[1] * seg[1] + seg[2] * seg[2];
            let seg_len = seg_len2.sqrt();
            if seg_len < 1e-9 {
                continue;
            }
            let d = [seg[0] / seg_len, seg[1] / seg_len, seg[2] / seg_len];
            let t_star = -(cam[0] * d[0] + cam[1] * d[1] + cam[2] * d[2]);
            let min_dist2 = if t_star < 0.0 {
                cam[0] * cam[0] + cam[1] * cam[1] + cam[2] * cam[2]
            } else if t_star > seg_len {
                p[0] * p[0] + p[1] * p[1] + p[2] * p[2]
            } else {
                cam[0] * cam[0] + cam[1] * cam[1] + cam[2] * cam[2] - t_star * t_star
            };
            if min_dist2 < 1.0 {
                continue; // occluded by Earth
            }

            // Project through view_proj (column-major mat4 ×
            // vec4(pos, 1.0)).
            let m = &view_proj;
            let x =
                (m[0] as f64) * p[0] + (m[4] as f64) * p[1] + (m[8] as f64) * p[2] + (m[12] as f64);
            let y =
                (m[1] as f64) * p[0] + (m[5] as f64) * p[1] + (m[9] as f64) * p[2] + (m[13] as f64);
            let _z = (m[2] as f64) * p[0]
                + (m[6] as f64) * p[1]
                + (m[10] as f64) * p[2]
                + (m[14] as f64);
            let w = (m[3] as f64) * p[0]
                + (m[7] as f64) * p[1]
                + (m[11] as f64) * p[2]
                + (m[15] as f64);
            if w <= 1e-9 {
                continue; // behind / on the camera plane
            }
            let ndc_x = x / w;
            let ndc_y = y / w;
            // NDC → screen pixels. NDC y is up; screen y is down.
            let sx = (ndc_x * 0.5 + 0.5) * canvas_w;
            let sy = (1.0 - (ndc_y * 0.5 + 0.5)) * canvas_h;
            let dx = sx - cursor_px.0;
            let dy = sy - cursor_px.1;
            let r2 = dx * dx + dy * dy;
            if r2 > pick_r2 {
                continue;
            }
            if best.is_none_or(|(_, b)| r2 < b) {
                best = Some((norad, r2));
            }
        }
        best.map(|(n, _)| n)
    }

    /// Advance the simulation clock by the monotonic delta since
    /// the last call, propagate every enabled satellite to that
    /// time, apply the budget guard if the count exceeds the cap,
    /// and upload the resulting instance buffer to the GPU.
    pub fn tick_orbit(&mut self, mono_now_s: f64) {
        self.sim_clock.step(mono_now_s);
        if self.satellites.is_empty() || self.active_body != BodyId::Earth {
            self.orbit_instance_count = 0;
            self.orbit_demoted_category = None;
            return;
        }
        let sim_t = self.sim_clock.sim_unix_s();

        // Apply the budget guard. Priority order (highest to lowest):
        // Stations → GNSS → Weather → Starlink → Debris → Other.
        // If the user enables more than the cap allows, suppress
        // categories from the bottom of that list.
        let priority: [Category; 6] = [
            Category::Stations,
            Category::Gnss,
            Category::Weather,
            Category::Starlink,
            Category::Debris,
            Category::Other,
        ];
        let mut allowed: HashSet<Category> = HashSet::new();
        let mut running_count = 0u32;
        let mut demoted: Option<Category> = None;
        for cat in priority {
            if !self.enabled_categories.contains(&cat) {
                continue;
            }
            let cat_count = self.satellites.iter().filter(|s| s.category == cat).count() as u32;
            if running_count + cat_count > self.orbit_budget_cap {
                if demoted.is_none() {
                    demoted = Some(cat);
                }
                continue;
            }
            allowed.insert(cat);
            running_count += cat_count;
        }
        self.orbit_demoted_category = demoted;

        // Propagate into a local Vec, then upload. Also record
        // (norad, position) into `orbit_frame_positions` for
        // CPU-side hit-testing in `satellite_under_cursor`.
        let mut instances: Vec<OrbitInstance> = Vec::with_capacity(running_count as usize);
        self.orbit_frame_positions.clear();
        self.orbit_frame_positions.reserve(running_count as usize);
        for sat in &self.satellites {
            if !allowed.contains(&sat.category) {
                continue;
            }
            if self.hidden_satellites.contains(&sat.norad_id) {
                continue;
            }
            let Some(pos) = orbit::propagate_render_space(sat, sim_t) else {
                continue;
            };
            let [r, g, b] = sat.category.color_srgb8();
            let highlight = if Some(sat.norad_id) == self.hovered_satellite {
                1.0
            } else {
                0.0
            };
            instances.push(OrbitInstance {
                world_pos: pos,
                color: [srgb8_to_linear(r), srgb8_to_linear(g), srgb8_to_linear(b)],
                highlight,
            });
            self.orbit_frame_positions.push((sat.norad_id, pos));
        }
        let needed = instances.len() as u32;
        if needed > self.orbit_instance_capacity {
            // Grow with headroom so we don't reallocate on every
            // single new TLE.
            let new_cap = (needed.next_power_of_two()).max(64);
            self.orbit_instance_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aegis-orbit-instances"),
                size: (new_cap as u64) * (std::mem::size_of::<OrbitInstance>() as u64),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.orbit_instance_capacity = new_cap;
        }
        self.queue.write_buffer(
            &self.orbit_instance_buf,
            0,
            bytemuck::cast_slice(&instances),
        );
        self.orbit_instance_count = needed;

        // Extend the camera's zoom-out range when at least one
        // satellite is visible — high-altitude orbits (GNSS ~3.2
        // Earth radii, geostationary ~5.6) need a wider view than
        // the default `MIN_ZOOM = 0.0` cap allows. When nothing
        // satellite-related is on screen the cap reverts so the
        // extra range isn't visible UX noise. -3.0 lets the camera
        // pull back to D = 9 (geostationary visible with margin).
        let extend_floor = self.orbit_instance_count > 0;
        let target_floor = if extend_floor {
            -3.0
        } else {
            crate::camera::MIN_ZOOM
        };
        if (self.camera.min_zoom - target_floor).abs() > 1e-9 {
            self.camera.min_zoom = target_floor;
            // Clamp the live zoom so re-hiding all satellites snaps
            // us back inside the normal range instead of leaving
            // the camera floating past z = 0 with no way to recover.
            self.camera.zoom = self
                .camera
                .zoom
                .clamp(target_floor, crate::camera::MAX_ZOOM);
        }

        // Update the orbit-trail vertex buffer (plan 0004 M3,
        // refined): a faint trail for every satellite in a
        // "small" enabled category (≤ TRAIL_CATEGORY_CAP — i.e.
        // Stations, Weather, GNSS but not Starlink/Debris), plus
        // a brighter trail for the selected satellite even if its
        // category is large. LineList topology: each segment is
        // a pair of consecutive samples → (N-1) × 2 vertices per
        // trail.
        const TRAIL_SAMPLES: usize = 128;
        const TRAIL_CATEGORY_CAP: usize = 200;
        let mut cat_counts: HashMap<Category, usize> = HashMap::new();
        for sat in &self.satellites {
            *cat_counts.entry(sat.category).or_insert(0) += 1;
        }

        let mut trail_verts: Vec<OrbitTrailVertex> = Vec::new();
        // Toggle gate — when `trails_enabled` is off, skip the
        // entire trail-build loop so we don't spend ~150 sgp4
        // calls per frame for trails the user disabled.
        if !self.trails_enabled {
            self.orbit_trail_vertex_count = 0;
            return;
        }

        // Faint trails for every small-category, allowed,
        // not-selected satellite.
        for sat in &self.satellites {
            if !allowed.contains(&sat.category) {
                continue;
            }
            if self.hidden_satellites.contains(&sat.norad_id) {
                continue;
            }
            if cat_counts.get(&sat.category).copied().unwrap_or(0) > TRAIL_CATEGORY_CAP {
                continue;
            }
            if Some(sat.norad_id) == self.selected_satellite {
                continue; // bright trail handled below
            }
            let [r, g, b] = sat.category.color_srgb8();
            let color = [
                srgb8_to_linear(r),
                srgb8_to_linear(g),
                srgb8_to_linear(b),
                0.25,
            ];
            let points = sat.trail_points(sim_t, TRAIL_SAMPLES);
            for window in points.windows(2) {
                trail_verts.push(OrbitTrailVertex {
                    position: window[0],
                    color,
                });
                trail_verts.push(OrbitTrailVertex {
                    position: window[1],
                    color,
                });
            }
        }

        // Bright trail for the selected satellite (any category).
        // Hidden satellites don't draw their trail even when
        // selected — that would be visually confusing ("the trail
        // is here but the dot isn't").
        if let Some(norad) = self.selected_satellite {
            if let Some(sat) = self.satellites.iter().find(|s| s.norad_id == norad) {
                if allowed.contains(&sat.category)
                    && !self.hidden_satellites.contains(&sat.norad_id)
                {
                    let [r, g, b] = sat.category.color_srgb8();
                    let color = [
                        srgb8_to_linear(r),
                        srgb8_to_linear(g),
                        srgb8_to_linear(b),
                        0.85,
                    ];
                    let points = sat.trail_points(sim_t, TRAIL_SAMPLES);
                    for window in points.windows(2) {
                        trail_verts.push(OrbitTrailVertex {
                            position: window[0],
                            color,
                        });
                        trail_verts.push(OrbitTrailVertex {
                            position: window[1],
                            color,
                        });
                    }
                }
            }
        }

        let n = trail_verts.len() as u32;
        if n > self.orbit_trail_vertex_capacity {
            let new_cap = (n.next_power_of_two()).max(256);
            self.orbit_trail_vertex_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("aegis-orbit-trail-verts"),
                size: (new_cap as u64) * (std::mem::size_of::<OrbitTrailVertex>() as u64),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.orbit_trail_vertex_capacity = new_cap;
        }
        if n > 0 {
            self.queue.write_buffer(
                &self.orbit_trail_vertex_buf,
                0,
                bytemuck::cast_slice(&trail_verts),
            );
        }
        self.orbit_trail_vertex_count = n;
    }

    /// Current simulation time in UNIX seconds (UTC).
    pub fn sim_unix_s(&self) -> f64 {
        self.sim_clock.sim_unix_s()
    }

    /// Set the simulation playback rate. `1.0` = real time;
    /// future plan 0010 time-slider UI drives this.
    pub fn set_sim_rate(&mut self, rate: f64) {
        self.sim_clock.set_rate(rate);
    }

    /// Jump the simulation clock to `unix_s` UTC seconds. The
    /// monotonic playback continues from there at the current
    /// rate, so the terminator + atmosphere + satellite positions
    /// all respond immediately and then keep drifting. Plan 0010.
    pub fn set_sim_unix_s(&mut self, unix_s: f64) {
        self.sim_clock.set_sim(unix_s);
    }

    /// Camera altitude above the planet surface in unit-sphere
    /// units. Useful for JS-side gating of globe-view-only UI
    /// (the time-slider panel, etc.) without re-deriving the
    /// formula.
    pub fn camera_altitude(&self) -> f64 {
        self.camera.altitude(self.size())
    }

    /// Headless "search and go" — parses `query` as either a
    /// coordinate expression or a geocoder query, picks the first
    /// result, and kicks off a fly-to to it. Native only;
    /// blocking. On web the equivalent flow lives in
    /// `src/web.rs`'s search-bar handler (M2).
    ///
    /// Returns the `SearchResult` that the camera is flying to,
    /// or `None` for a coord-parseable input (no geocoder result).
    /// `Err` if the query was unparseable AND the geocoder
    /// returned no results.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn search_and_fly_to(
        &mut self,
        query: &str,
        now: f64,
    ) -> Result<Option<crate::search::SearchResult>, crate::search::GeocodeError> {
        // Coord path first — offline and unambiguous when it matches.
        if let Some((lon, lat)) = crate::search::parse_coord(query) {
            self.fly_to((lon, lat), 12.0, now);
            return Ok(None);
        }
        let mut client = crate::search::GeocoderClient::new();
        let near = Some(self.camera.center_lonlat);
        let results = crate::search::geocode_blocking(&mut client, query, near)?;
        let Some(first) = results.into_iter().next() else {
            return Err(crate::search::GeocodeError::Malformed);
        };
        match first.bbox {
            Some(b) => self.fly_to_bbox(b, now),
            None => self.fly_to(first.lonlat, first.kind.default_zoom(), now),
        }
        Ok(Some(first))
    }

    /// Native: spawn a thread per satellite-tile request.
    #[cfg(not(target_arch = "wasm32"))]
    fn dispatch_sat_tile_fetch(&self, id: TileId) {
        let tx = self.sat_completed_tx.clone();
        let url = body::format_tile_url(self.active_basemap_ref().url_template, id.z, id.x, id.y);
        std::thread::spawn(move || {
            let result = tile::fetch_tile_blocking(&url);
            let _ = tx.send((id, result));
        });
    }

    /// Web: spawn a JS-event-loop task per satellite-tile request.
    #[cfg(target_arch = "wasm32")]
    fn dispatch_sat_tile_fetch(&self, id: TileId) {
        let tx = self.sat_completed_tx.clone();
        let url = body::format_tile_url(self.active_basemap_ref().url_template, id.z, id.x, id.y);
        wasm_bindgen_futures::spawn_local(async move {
            let result = tile::fetch_tile_web(&url).await;
            let _ = tx.send((id, result));
        });
    }

    /// Draw one frame.
    ///
    /// Single-projection 3D scene: every vertex (tile + vector)
    /// projects its sphere position through the camera's
    /// view-projection matrix. The flat-slippy-map look at high zoom
    /// emerges from the camera being close to the sphere surface
    /// (~2% above at z=10); the full globe view at low zoom emerges
    /// from the camera being far enough out to see the whole sphere.
    pub fn render(&self) {
        let canvas = self.size();
        // When a city's building mesh is loaded, pull the near plane
        // in below its tallest building so the top doesn't clip at
        // street zoom (plan 0014 § Coordinate frame for extrusion).
        // Half the max height gives a comfortable margin; with no
        // buildings loaded we pass 0.0 and the call falls through
        // to the existing `altitude * 0.1` near.
        let min_near_floor = self
            .building_binding
            .as_ref()
            .map(|b| b.max_height_world * 0.5)
            .unwrap_or(0.0);
        let view_proj = self
            .camera
            .view_projection_matrix_with_floor(canvas, min_near_floor);
        let camera_pos = self.camera.camera_3d_position(canvas);
        // Day/night state shared across every body-surface uniform
        // upload this frame (plan 0009 M0). The sun direction is
        // pulled from `SimClock` so that, once the time slider
        // (plan 0010) lands, scrubbing time also scrubs the
        // terminator. `night_dim` is the per-body intensity floor
        // on the night side — Earth 0.15, Mars 0.10, Moon 0.02.
        let sun_dir = crate::sun::direction_from_unix(self.sim_clock.sim_unix_s());
        let night_dim = self.active_body_ref().night_dim;

        // Tile draws: every loaded tile at or below the current
        // rounded camera zoom. Filtering out deeper-than-current
        // tiles is the fix for "zoom in, then zoom out, and tile
        // text stays tiny": when the user zoomed in we loaded deep
        // tiles whose native text size is sized for that zoom; on
        // zoom-out those tiles are still in `self.tiles` and used
        // to draw on top of the parents, painting small screen
        // slivers with text that's now far too small. Skipping
        // them here keeps the on-screen text scaled to the current
        // zoom (parent tiles fill in until the new "right" zoom
        // loads).
        //
        // We don't evict from `self.tiles`; the deeper bindings
        // stay cached on the GPU so re-zooming-in finds them
        // already uploaded.
        let current_z = self.camera.zoom.round().clamp(0.0, crate::camera::MAX_ZOOM) as u8;
        // The Map cache lives on Earth's "map" basemap which is
        // always WebMercator; pin that explicitly so the projection-
        // kind uniform agrees with the tile-rect math.
        let map_projection = crate::body::EARTH.basemap(BasemapId("map")).projection;
        let map_projection_kind = projection_to_u32(map_projection);
        let mut draws: Vec<(&TileId, [f32; 4], &TileBinding)> = self
            .tiles
            .iter()
            .filter(|(id, _)| id.z <= current_z)
            .map(|(id, binding)| (id, tile::tile_world_rect(map_projection, *id), binding))
            .collect();
        // Coarse-first: finer tiles overdraw their parents.
        draws.sort_by_key(|(id, _, _)| id.z);

        // Tile alpha fades out at globe view so the earth.wgsl
        // composite (Blue Marble + Black Marble + day/night
        // terminator) is the visible surface there. Without this
        // fade, opaque tiles overdraw the lit globe at low zoom
        // and the city-lights texture appears to blink in and out
        // as tiles stream in — plan 0009 M2 follow-up.
        //
        // Window matches `day_night_color`'s `smoothstep(0.05, 0.5)`
        // so the dim ramp on tiles + the alpha ramp + the globe
        // composite's strength term all step in lockstep. The
        // basemap toggle (Map ↔ Satellite) is still visible past
        // z ≈ 4 where alpha > 0; at globe view the toggle has no
        // visible effect, which is fine — the user sees Earth-
        // from-space regardless.
        let cam_alt =
            (camera_pos[0].powi(2) + camera_pos[1].powi(2) + camera_pos[2].powi(2)).sqrt() - 1.0;
        let tile_alpha = 1.0 - smoothstep_f32(0.05, 0.5, cam_alt);

        for (_, world_rect, binding) in &draws {
            let u = TileUniforms {
                view_proj,
                camera_pos,
                tile_alpha,
                world_rect: *world_rect,
                projection_kind: map_projection_kind,
                _pad: [0; 3],
                sun_dir,
                night_dim,
            };
            self.queue
                .write_buffer(&binding.uniform_buf, 0, bytemuck::bytes_of(&u));
        }

        // Satellite-tile draws — every loaded Esri World Imagery tile
        // at or below the current camera zoom gets queued, sorted
        // coarse-first so finer tiles overdraw their parents. Without
        // the filter + sort, a stale parent tile cached from a prior
        // zoom-out can land *on top* of a freshly-fetched child at
        // the current zoom, producing a checkerboard of crisp + blurry
        // squares (a loaded high-z tile randomly hidden behind its
        // own parent). Same pattern as the Carto path above; the
        // satellite cache needs the same discipline.
        // The Satellite cache uses the active basemap's projection
        // (Earth/satellite is WebMercator; Mars/Moon basemaps in
        // M2/M3 are Equirectangular). The world_rect math and the
        // shader's inverse both pivot on this single value.
        let sat_basemap = self.active_basemap_ref();
        let sat_projection_kind = projection_to_u32(sat_basemap.projection);
        let sat_current_z = self.camera.zoom.round().clamp(0.0, crate::camera::MAX_ZOOM) as u8;
        let mut sat_draws: Vec<(&TileId, [f32; 4], &TileBinding)> = self
            .sat_tiles
            .iter()
            .filter(|(id, _)| id.z <= sat_current_z)
            .map(|(id, binding)| {
                (
                    id,
                    tile::tile_world_rect(sat_basemap.projection, *id),
                    binding,
                )
            })
            .collect();
        sat_draws.sort_by_key(|(id, _, _)| id.z);
        for (_, world_rect, binding) in &sat_draws {
            let u = TileUniforms {
                view_proj,
                camera_pos,
                tile_alpha,
                world_rect: *world_rect,
                projection_kind: sat_projection_kind,
                _pad: [0; 3],
                sun_dir,
                night_dim,
            };
            self.queue
                .write_buffer(&binding.uniform_buf, 0, bytemuck::bytes_of(&u));
        }

        // Per-frame vector-camera uniform. 112 bytes post day/night.
        let vector_camera = VectorCameraUniform {
            view_proj,
            position: camera_pos,
            _pad0: 0.0,
            // Country-outline overlay colour: coral-orange that reads
            // against OSM's brown/beige basemap. Alpha kept moderate
            // so the basemap shows faintly under the lines.
            color: [0.95, 0.42, 0.22, 0.85],
            sun_dir,
            night_dim,
        };
        self.queue.write_buffer(
            &self.vector_camera_buf,
            0,
            bytemuck::bytes_of(&vector_camera),
        );

        // Sibling uniform for the click-highlight draw. Same
        // camera + sun state, different colour — bright cyan that
        // reads against both the orange country outlines and the
        // basemap imagery underneath.
        let highlight_uniform = VectorCameraUniform {
            view_proj,
            position: camera_pos,
            _pad0: 0.0,
            color: [0.15, 0.95, 0.95, 0.95],
            sun_dir,
            night_dim,
        };
        self.queue.write_buffer(
            &self.highlight_camera_buf,
            0,
            bytemuck::bytes_of(&highlight_uniform),
        );

        // Per-frame cap uniforms. Same view_proj + camera_pos as the
        // other passes; the per-cap `pole_sign` and `color` are baked
        // into each buffer.
        //
        // Cap colours: in Carto Map mode we keep the hand-picked
        // stylised palette (Arctic blue + Antarctic cream) because
        // there's no underlying imagery to match. Everywhere else
        // we use the polar-row average of the body's fallback
        // texture — that way the cap blends into the imagery
        // instead of sitting on top as a distinct disc. The colour
        // is computed once at body-load (`build_body_resources`).
        let basemap = self.active_basemap_ref();
        let (north_rgb8, south_rgb8) = if self.is_carto_map_mode() {
            (basemap.cap_colors.north, basemap.cap_colors.south)
        } else if let Some(res) = self.body_resources.get(&self.active_body) {
            (res.north_polar_color, res.south_polar_color)
        } else {
            (basemap.cap_colors.north, basemap.cap_colors.south)
        };
        let [nr, ng, nb, na] = north_rgb8;
        let [sr, sg, sb, sa] = south_rgb8;
        let north_cap_color = srgb8_to_linear_rgba(nr, ng, nb, na);
        let south_cap_color = srgb8_to_linear_rgba(sr, sg, sb, sa);
        let north_cap = CapUniform {
            view_proj,
            camera_pos,
            pole_sign: 1.0,
            color: north_cap_color,
            sun_dir,
            night_dim,
        };
        let south_cap = CapUniform {
            view_proj,
            camera_pos,
            pole_sign: -1.0,
            color: south_cap_color,
            sun_dir,
            night_dim,
        };
        self.queue
            .write_buffer(&self.north_cap_buf, 0, bytemuck::bytes_of(&north_cap));
        self.queue
            .write_buffer(&self.south_cap_buf, 0, bytemuck::bytes_of(&south_cap));

        // Body-texture camera uniform — view_proj + position (used
        // for the back-hemisphere discard in earth.wgsl). Written
        // into the active body's per-body camera buffer; other
        // bodies' buffers are untouched (cheap — they don't render
        // this frame).
        let body_camera = EarthCameraUniform {
            view_proj,
            position: camera_pos,
            _pad0: 0.0,
            sun_dir,
            night_dim,
        };
        if let Some(res) = self.body_resources.get(&self.active_body) {
            self.queue
                .write_buffer(&res.camera_buf, 0, bytemuck::bytes_of(&body_camera));
        }

        // Starfield uniform. Camera basis is re-derived in-shader
        // from `camera_pos` (target = origin), so we only need to
        // pass the position + aspect + up-hint + a zoom-driven
        // strength. The up-hint mirrors the same near-pole switch
        // that `view_projection_matrix` uses so the cross product
        // producing `right` doesn't degenerate at the poles.
        let star_strength = smoothstep_f32(0.05, 0.5, cam_alt);
        let up_hint = if self.camera.center_lonlat.1.abs() > 89.0 {
            [0.0_f32, 0.0, 1.0]
        } else {
            [0.0_f32, 1.0, 0.0]
        };
        let aspect = canvas.0 as f32 / canvas.1.max(1) as f32;
        let star_uniform = StarfieldUniform {
            camera_pos,
            aspect,
            up_hint,
            strength: star_strength,
            sun_dir,
            _pad: 0.0,
        };
        self.queue.write_buffer(
            &self.starfield_uniform_buf,
            0,
            bytemuck::bytes_of(&star_uniform),
        );

        // Atmosphere uniform (plan 0008 M1+M2). Skipped entirely
        // when the active body has no atmosphere (Moon); otherwise
        // the per-body Rayleigh + Mie params are merged with the
        // shared sun + camera state and a zoom-driven strength.
        // The strength shares the `smoothstep(0.05, 0.5)` window
        // with `day_night_color` so atmosphere + terminator fade
        // in / out together at globe ↔ street zoom.
        if let Some(atm) = self.active_body_ref().atmosphere {
            let atm_uniform = AtmosphereUniform {
                view_proj,
                camera_pos,
                planet_radius: 1.0,
                sun_dir,
                atmosphere_radius: atm.atmosphere_radius,
                rayleigh_beta: atm.rayleigh_beta,
                sun_intensity: atm.sun_intensity,
                mie_beta: atm.mie_beta,
                mie_g: atm.mie_g,
                rayleigh_scale: atm.rayleigh_scale,
                mie_scale: atm.mie_scale,
                strength: smoothstep_f32(0.05, 0.5, cam_alt),
                _pad: 0.0,
            };
            self.queue.write_buffer(
                &self.atmosphere_uniform_buf,
                0,
                bytemuck::bytes_of(&atm_uniform),
            );
        }

        // Building uniform (plan 0014 M1). Strength is zoom-gated
        // by `smoothstep(14.0, 15.5, zoom)` so buildings fade in
        // across that band rather than popping. Per-body fill +
        // wall colours come from `Body::buildings`. The draw call
        // gate (further down) skips entirely when buildings are
        // off-body or unloaded — the uniform write is harmless
        // either way.
        let zoom = self.camera.zoom as f32;
        let building_strength = smoothstep_f32(14.0, 15.5, zoom);
        let (fill_color, wall_color) = self
            .active_body_ref()
            .buildings
            .map(|s| (s.fill_color, s.wall_color))
            .unwrap_or(([0.0; 4], [0.0; 4]));
        let building_uniform = BuildingUniforms {
            view_proj,
            camera_pos,
            strength: building_strength,
            sun_dir,
            night_dim,
            fill_color,
            wall_color,
        };
        self.queue.write_buffer(
            &self.building_uniform_buf,
            0,
            bytemuck::bytes_of(&building_uniform),
        );

        // Orbit overlay camera (plan 0004 M1). Same view_proj +
        // camera_pos as the other passes, plus viewport pixels for
        // the billboard pixel-size math in the shader.
        let orbit_camera = OrbitCameraUniform {
            view_proj,
            camera_pos,
            _pad0: 0.0,
            viewport_px: [canvas.0 as f32, canvas.1 as f32],
            _pad1: [0.0, 0.0],
        };
        self.queue
            .write_buffer(&self.orbit_camera_buf, 0, bytemuck::bytes_of(&orbit_camera));

        // Orbit trail uniform (plan 0004 M3, refined). Trail
        // colour now lives in per-vertex attributes, so the
        // uniform is just view + camera position.
        let orbit_trail_uniform = OrbitTrailUniform {
            view_proj,
            camera_pos,
            _pad0: 0.0,
        };
        self.queue.write_buffer(
            &self.orbit_trail_uniform_buf,
            0,
            bytemuck::bytes_of(&orbit_trail_uniform),
        );

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
            wgpu::CurrentSurfaceTexture::Validation => {
                log::warn!("surface validation error acquiring frame");
                return;
            }
        };
        // Cast the surface texture to its sRGB-encoded view (declared
        // via `view_formats` in the surface configuration). The GPU
        // applies linear → sRGB encoding on writeout against this
        // view, so the canvas stores correctly gamma-encoded bytes
        // even when the surface's native format is linear. Without
        // this cast, our linear-light shader output gets stored
        // verbatim and the browser displays it as raw sRGB bytes —
        // visibly dim across every pipeline.
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.view_format),
            ..wgpu::TextureViewDescriptor::default()
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("aegis-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("aegis-main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // Background colour (linear-space coords for an
                        // sRGB display target of #0b0e12 — matches the
                        // UI chrome in index.html).
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0144,
                            g: 0.0181,
                            b: 0.0241,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Procedural starfield — drawn first so the lit globe +
            // atmosphere overdraw it where they cover the canvas.
            // Strength ramps to 0 at street zoom; below that the
            // fragment shader early-discards so we don't pay the
            // per-pixel hash cost when stars wouldn't be visible
            // anyway.
            if star_strength > 0.0 {
                pass.set_pipeline(&self.starfield_pipeline);
                pass.set_bind_group(0, &self.starfield_bind_group, &[]);
                pass.draw(0..6, 0..1);
            }

            // Body fallback texture — bundled equirectangular imagery
            // covering the full sphere. Drawn under every basemap
            // except Earth's "map" (Carto's opaque tiles already
            // cover the world at every zoom, so the fallback would
            // just burn fragments). For every other basemap the
            // streaming tiles overdraw it where they've loaded;
            // anywhere they haven't, the fallback is the surface
            // the user sees.
            let draw_fallback =
                !(self.active_body == BodyId::Earth && self.active_basemap == BasemapId("map"));
            if draw_fallback {
                if let Some(res) = self.body_resources.get(&self.active_body) {
                    pass.set_pipeline(&self.body_pipeline);
                    pass.set_bind_group(0, &res.bind_group, &[]);
                    pass.draw(0..EARTH_DRAW_VERTS, 0..1);
                }
            }

            if self.is_carto_map_mode() && !draws.is_empty() {
                pass.set_pipeline(&self.tile_pipeline);
                // 32×32 grid of quads × 6 verts/quad — matches `GRID`
                // + `QUAD_VERTS` in tile.wgsl. The grid is what lets
                // each tile curve onto the globe at low zoom rather
                // than rendering as a flat NDC quad. Higher GRID =
                // smoother silhouette when one tile covers the whole
                // sphere (z=0); cost is trivial at high zoom where
                // each tile covers a tiny region.
                const TILE_GRID_VERTS: u32 = 32 * 32 * 6;
                for (_, _, binding) in &draws {
                    pass.set_bind_group(0, &binding.bind_group, &[]);
                    pass.draw(0..TILE_GRID_VERTS, 0..1);
                }
            }

            // Satellite-tile layer — Satellite-only. Drawn over the
            // bundled Earth texture; tiles refine sharpness wherever
            // loaded. Same `tile_pipeline` as Carto since both are
            // Web Mercator.
            if !self.is_carto_map_mode() && !sat_draws.is_empty() {
                pass.set_pipeline(&self.tile_pipeline);
                const TILE_GRID_VERTS: u32 = 32 * 32 * 6;
                for (_, _, binding) in &sat_draws {
                    pass.set_bind_group(0, &binding.bind_group, &[]);
                    pass.draw(0..TILE_GRID_VERTS, 0..1);
                }
            }

            // Polar caps — drawn in both modes. They fill the
            // |lat| > 85.051° band that Web Mercator can't tile, and
            // (importantly) they also cover the degenerate-UV disc
            // the Blue Marble equirectangular texture produces at
            // the actual pole. Without the cap in Satellite mode a
            // small distorted disc of the texture was visible
            // through the south pole. The cap colours are mode-
            // specific so Satellite-mode caps match the real polar
            // imagery rather than the stylised Map palette.
            pass.set_pipeline(&self.cap_pipeline);
            pass.set_bind_group(0, &self.north_cap_bind_group, &[]);
            pass.draw(0..CAP_DRAW_VERTS, 0..1);
            pass.set_bind_group(0, &self.south_cap_bind_group, &[]);
            pass.draw(0..CAP_DRAW_VERTS, 0..1);

            // Extruded buildings (plan 0014 M1). Earth-only;
            // gated on zoom > 14 so we don't burn fragment work
            // when buildings are invisible anyway. The depth
            // attachment on this pass keeps building-vs-building
            // occlusion correct in the single indexed draw call.
            if let Some(binding) = &self.building_binding {
                if self.active_body == BodyId::Earth
                    && self.active_body_ref().buildings.is_some()
                    && self.camera.zoom > 14.0
                {
                    pass.set_pipeline(&self.building_pipeline);
                    pass.set_bind_group(0, &binding.bind_group, &[]);
                    pass.set_vertex_buffer(0, binding.vbo.slice(..));
                    pass.set_index_buffer(binding.ibo.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..binding.index_count, 0, 0..1);
                }
            }

            // Atmospheric-scattering shell (plan 0008 M1+M2). Drawn
            // additively after caps + tiles so the halo brightens
            // whatever surface is underneath. Bodies without an
            // atmosphere (Moon) skip the draw; the strength
            // uniform also gates at zero alpha so the fragment
            // shader discards early at street zoom.
            if self.active_body_ref().atmosphere.is_some() {
                pass.set_pipeline(&self.atmosphere_pipeline);
                pass.set_bind_group(0, &self.atmosphere_bind_group, &[]);
                pass.draw(0..ATMOSPHERE_DRAW_VERTS, 0..1);
            }

            // Vector overlay (Natural Earth country outlines) — only
            // makes sense on Earth. Mars / Moon / Middle-earth have
            // no countries, and rendering the Earth-shaped outline
            // over them would look wrong.
            if self.active_body_ref().show_political_overlays && self.borders_visible {
                if let Some(vector) = &self.vector {
                    pass.set_pipeline(&self.vector_pipeline);
                    pass.set_bind_group(0, &self.vector_bind_group, &[]);
                    pass.set_vertex_buffer(0, vector.vertex_buf.slice(..));
                    pass.draw(0..vector.vertex_count, 0..1);
                }
            }

            // Highlight the click-picked country outline. Drawn
            // after the orange outlines so the cyan overlay wins
            // where they overlap; bound to a sibling uniform
            // buffer carrying the highlight colour. Skips when
            // the user has toggled borders off — without the
            // base outline as visual context the highlight floats
            // on its own and reads as noise.
            if self.active_body_ref().show_political_overlays
                && self.borders_visible
                && self.selected_vector_count > 0
            {
                if let Some(buf) = &self.selected_vector_buf {
                    pass.set_pipeline(&self.vector_pipeline);
                    pass.set_bind_group(0, &self.highlight_bind_group, &[]);
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..self.selected_vector_count, 0..1);
                }
            }

            // Orbit trail (plan 0004 M3) — drawn before the points
            // so satellites render on top of their own trail. Only
            // shown when any curvature is in view (`globeness > 0`);
            // at flat-Mercator zoom the trail reads as a stray
            // yellow line crossing city imagery rather than an
            // orbital path, so we suppress it there.
            if self.active_body == BodyId::Earth
                && self.orbit_trail_vertex_count >= 2
                && self.camera.globeness() > 0.0
            {
                pass.set_pipeline(&self.orbit_trail_pipeline);
                pass.set_bind_group(0, &self.orbit_trail_bind_group, &[]);
                pass.set_vertex_buffer(0, self.orbit_trail_vertex_buf.slice(..));
                pass.draw(0..self.orbit_trail_vertex_count, 0..1);
            }

            // Satellite-orbit overlay — only on Earth, only when
            // any satellites are loaded. Each instance draws 6
            // verts (two triangles) for the billboarded point.
            // Plan 0004 M1.
            if self.active_body == BodyId::Earth && self.orbit_instance_count > 0 {
                pass.set_pipeline(&self.orbit_pipeline);
                pass.set_bind_group(0, &self.orbit_bind_group, &[]);
                pass.set_vertex_buffer(0, self.orbit_instance_buf.slice(..));
                pass.draw(0..6, 0..self.orbit_instance_count);
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
    }
}

fn build_tile_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aegis-tile-shader"),
        source: wgpu::ShaderSource::Wgsl(TILE_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aegis-tile-layout"),
        bind_group_layouts: &[Some(bgl)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aegis-tile-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                // Phase 9: alpha-blend so tiles can fade out by
                // `(1 - globeness)` during the flat → globe
                // transition. Standard SRC_ALPHA over (straight
                // alpha, not premultiplied).
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(pass_through_depth_state()),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn build_vector_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aegis-vector-shader"),
        source: wgpu::ShaderSource::Wgsl(VECTOR_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aegis-vector-layout"),
        bind_group_layouts: &[Some(bgl)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aegis-vector-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<[f32; 2]>() as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                }],
            }],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                // Straight alpha blend so the line colour's alpha
                // controls visibility over the basemap.
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::LineList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(pass_through_depth_state()),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn build_cap_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    bgl: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aegis-caps-shader"),
        source: wgpu::ShaderSource::Wgsl(CAPS_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aegis-caps-layout"),
        bind_group_layouts: &[Some(bgl)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aegis-caps-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(pass_through_depth_state()),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// Create the (buffer, bind-group) pair for one polar cap. `pole_sign`
/// stamps the initial uniform; `render()` overwrites view_proj +
/// camera_pos every frame but the pole_sign sticks for the lifetime
/// of the renderer (north == +1, south == −1).
fn make_cap_binding(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    label: &str,
    pole_sign: f32,
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let init = CapUniform {
        pole_sign,
        ..CapUniform::default()
    };
    let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytemuck::bytes_of(&init),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout: bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buf.as_entire_binding(),
        }],
    });
    (buf, bind_group)
}

/// Build the earth-texture pipeline together with its uniform buffer
/// and bind group. Decodes the bundled Blue Marble PNG, uploads it as
/// an sRGB 2D texture, and wires the per-frame camera uniform.
/// One body's fallback-texture GPU resources. Plan 0003 M2 makes
/// this per-body; the renderer holds a small map keyed by `BodyId`
/// and picks the active body's entry at draw time.
struct BodyResources {
    /// Held only to keep the texture view inside `bind_group`
    /// alive for the lifetime of the renderer.
    _texture: wgpu::Texture,
    /// Held to keep the bind-group's night-texture view alive.
    /// Bodies without a city-lights texture (Mars, Moon) still
    /// own a 1×1 black pixel so the BGL stays uniform.
    _night_texture: wgpu::Texture,
    _sampler: wgpu::Sampler,
    camera_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Average sRGB8 colour across the fallback texture's top row
    /// (lat = +90° in the equirectangular projection). The polar
    /// cap reads this so its colour matches the imagery it covers
    /// instead of a hand-picked guess.
    north_polar_color: [u8; 4],
    /// Same shape, bottom row (lat = -90°).
    south_polar_color: [u8; 4],
}

/// Channel-wise average of one row of an RGBA8 texture. Used to
/// derive cap colours from the fallback texture's polar rows so the
/// cap blends into the surrounding imagery instead of standing out
/// as a flat disc. Averaging in sRGB space slightly biases the
/// result toward darker tones, but the cap region's brightness is
/// already fairly uniform per body so the bias is invisible.
fn average_row_rgba8(rgba: &[u8], width: u32, row_y: u32) -> [u8; 4] {
    let stride = (width as usize) * 4;
    let start = row_y as usize * stride;
    let row = &rgba[start..start + stride];
    let n = width as u64;
    let mut sum = [0u64; 4];
    for px in row.chunks_exact(4) {
        for c in 0..4 {
            sum[c] += px[c] as u64;
        }
    }
    [
        (sum[0] / n) as u8,
        (sum[1] / n) as u8,
        (sum[2] / n) as u8,
        (sum[3] / n) as u8,
    ]
}

fn build_body_resources(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    body: &Body,
    bgl: &wgpu::BindGroupLayout,
) -> BodyResources {
    let decoded = tile::decode_image(body.fallback_texture).unwrap_or_else(|_| {
        panic!(
            "{} fallback JPEG failed to decode — binary is corrupt",
            body.display_name
        )
    });
    log::info!(
        "body fallback: {} decoded {}×{} from bundled JPEG",
        body.display_name,
        decoded.width,
        decoded.height
    );

    // Sample the polar rows for cap-colour derivation. Done before
    // the GPU upload so we don't have to ferry the bytes back later.
    let north_polar_color = average_row_rgba8(&decoded.rgba, decoded.width, 0);
    let south_polar_color = average_row_rgba8(&decoded.rgba, decoded.width, decoded.height - 1);
    log::info!(
        "body fallback: {} polar averages — N {:?}, S {:?}",
        body.display_name,
        north_polar_color,
        south_polar_color
    );

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("aegis-body-texture"),
        size: wgpu::Extent3d {
            width: decoded.width,
            height: decoded.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // sRGB so the PNG-source colour space matches what the sRGB
        // surface expects — same convention the tile pipeline uses.
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &decoded.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(decoded.width * 4),
            rows_per_image: Some(decoded.height),
        },
        wgpu::Extent3d {
            width: decoded.width,
            height: decoded.height,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("aegis-body-sampler"),
        // U wraps so the seam at lon = ±180° interpolates correctly
        // across the antimeridian; V clamps so the polar rows don't
        // smear past the texture top/bottom.
        address_mode_u: wgpu::AddressMode::Repeat,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });

    // Night-side city-lights texture (plan 0009 M2). Earth ships
    // NASA Black Marble; other bodies bind a 1×1 black pixel so
    // the bind-group layout stays uniform. The fragment shader
    // composites this on top of the dimmed day surface on the
    // night hemisphere (additive blend, so a black sampling falls
    // back cleanly to the M1 behaviour).
    let (night_rgba, night_w, night_h) = if let Some(bytes) = body.night_texture {
        let decoded = tile::decode_image(bytes).unwrap_or_else(|_| {
            panic!(
                "{} night-lights JPEG failed to decode — binary is corrupt",
                body.display_name
            )
        });
        log::info!(
            "body night-lights: {} decoded {}×{} from bundled JPEG",
            body.display_name,
            decoded.width,
            decoded.height
        );
        (decoded.rgba, decoded.width, decoded.height)
    } else {
        (vec![0u8, 0, 0, 255], 1u32, 1u32)
    };
    let night_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("aegis-body-night-texture"),
        size: wgpu::Extent3d {
            width: night_w,
            height: night_h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &night_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &night_rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(night_w * 4),
            rows_per_image: Some(night_h),
        },
        wgpu::Extent3d {
            width: night_w,
            height: night_h,
            depth_or_array_layers: 1,
        },
    );
    let night_view = night_texture.create_view(&wgpu::TextureViewDescriptor::default());

    let camera_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("aegis-body-camera"),
        contents: bytemuck::bytes_of(&EarthCameraUniform::default()),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("aegis-body-bg"),
        layout: bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(&night_view),
            },
        ],
    });
    BodyResources {
        _texture: texture,
        _night_texture: night_texture,
        _sampler: sampler,
        camera_buf,
        bind_group,
        north_polar_color,
        south_polar_color,
    }
}

/// Build the shared pipeline + bind-group layout used by every body's
/// fallback-texture draw. Resources differ per body (`build_body_resources`),
/// but the shader + pipeline state are identical.
fn build_body_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aegis-body-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aegis-body-shader"),
        source: wgpu::ShaderSource::Wgsl(EARTH_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aegis-body-layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aegis-body-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(pass_through_depth_state()),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, bgl)
}

/// Build the procedural-starfield pipeline + uniform buffer. One
/// fullscreen quad (6 vertices, no buffer) renders first in the
/// pass so subsequent draws can overwrite the stars where they
/// cover the view.
/// Build the extruded-building pipeline + the shared uniform
/// buffer. Returns the pipeline, BGL (cached for re-binding when
/// `load_buildings` rebuilds the per-city storage buffer), and the
/// shared uniform buffer (rewritten each frame). Plan 0014 M1.
fn build_building_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout, wgpu::Buffer) {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aegis-building-bgl"),
        entries: &[
            // Uniform (camera + sun + per-body colour + strength).
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // Per-building storage buffer (centroid normals).
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aegis-building-shader"),
        source: wgpu::ShaderSource::Wgsl(BUILDING_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aegis-building-layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    // Vertex layout: world (vec2) + height_world (f32) +
    // building_idx (u32) + face_kind (u32) + normal (vec3). 32 B
    // total. Matches `BuildingVertex` in `crate::buildings`.
    let vertex_buffer_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<crate::buildings::BuildingVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &[
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32,
                offset: 8,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 12,
                shader_location: 2,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 16,
                shader_location: 3,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 20,
                shader_location: 4,
            },
        ],
    };
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aegis-building-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[vertex_buffer_layout],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None, // walls + roofs both visible from above
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(building_depth_state()),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let uniform_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("aegis-building-uniform"),
        contents: bytemuck::bytes_of(&BuildingUniforms::default()),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    (pipeline, bgl, uniform_buf)
}

fn build_starfield_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> (wgpu::RenderPipeline, wgpu::Buffer, wgpu::BindGroup) {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aegis-starfield-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aegis-starfield-shader"),
        source: wgpu::ShaderSource::Wgsl(STARFIELD_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aegis-starfield-layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aegis-starfield-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                // Pre-multiplied alpha — the shader outputs
                // (color * strength, strength) so this blend gives
                // stars at strength=1 and a clean fade to the
                // cleared background at strength=0.
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent::OVER,
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(pass_through_depth_state()),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let uniform_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("aegis-starfield-uniform"),
        contents: bytemuck::bytes_of(&StarfieldUniform::default()),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("aegis-starfield-bg"),
        layout: &bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buf.as_entire_binding(),
        }],
    });
    (pipeline, uniform_buf, bind_group)
}

/// Standard `smoothstep` (Hermite interpolation, 3t² − 2t³). Used
/// by the tile-alpha zoom fade so the camera-altitude → opacity
/// curve matches the WGSL `day_night_color` ramp in lockstep.
fn smoothstep_f32(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Build the atmospheric-scattering pipeline + one shared uniform
/// buffer (rewritten each frame for the active body when its
/// `Body::atmosphere` is Some). Plan 0008 M1+M2.
///
/// The pipeline draws a procedural sphere mesh at the body's
/// atmosphere radius (front face only — cull `Back` since the
/// camera is outside the shell at any zoom where strength > 0)
/// and additively blends the per-fragment scattered light onto
/// the planet underneath.
///
/// Vertex count for the draw call: `LAT_BANDS * LON_SEGMENTS *
/// QUAD_VERTS` in atmosphere.wgsl (48 × 96 × 6 = 27 648).
fn build_atmosphere_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> (wgpu::RenderPipeline, wgpu::Buffer, wgpu::BindGroup) {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aegis-atmosphere-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aegis-atmosphere-shader"),
        source: wgpu::ShaderSource::Wgsl(ATMOSPHERE_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aegis-atmosphere-layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aegis-atmosphere-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                // Additive blend — the atmosphere's scattered light
                // adds to whatever's behind. Pre-multiplied alpha
                // out of the shader (`color * alpha, alpha`) keeps
                // the limb halo from over-saturating when stacked
                // on the lit globe.
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::One,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::One,
                        operation: wgpu::BlendOperation::Add,
                    },
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: Some(wgpu::Face::Back),
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(pass_through_depth_state()),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let uniform_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("aegis-atmosphere-uniform"),
        contents: bytemuck::bytes_of(&AtmosphereUniform::default()),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("aegis-atmosphere-bg"),
        layout: &bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buf.as_entire_binding(),
        }],
    });
    (pipeline, uniform_buf, bind_group)
}

/// Convert an 8-bit sRGB channel to linear-light. Used so cap (and
/// future overlay) colours can live in the source as the same RGB-8
/// triples a paint picker or hex code would surface, while the GPU
/// receives the linear values the sRGB surface expects to
/// gamma-encode on output. Matches the IEC 61966-2-1 piecewise
/// transfer function exactly — the same formula Chrome, Firefox,
/// and `wgpu::TextureFormat::Rgba8UnormSrgb` apply internally.
fn srgb8_to_linear(c: u8) -> f32 {
    let s = c as f32 / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// Build the satellite-orbit pipeline + initial instance buffer.
/// Returns `(pipeline, bgl, camera_buf, bind_group, instance_buf)`.
/// Plan 0004 M1.
fn build_orbit_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    initial_capacity: u32,
) -> (
    wgpu::RenderPipeline,
    wgpu::BindGroupLayout,
    wgpu::Buffer,
    wgpu::BindGroup,
    wgpu::Buffer,
) {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aegis-orbit-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let camera_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("aegis-orbit-camera"),
        contents: bytemuck::bytes_of(&OrbitCameraUniform::default()),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("aegis-orbit-bg"),
        layout: &bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: camera_buf.as_entire_binding(),
        }],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aegis-orbit-shader"),
        source: wgpu::ShaderSource::Wgsl(ORBIT_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aegis-orbit-layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aegis-orbit-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<OrbitInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 12,
                        shader_location: 1,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32,
                        offset: 24,
                        shader_location: 2,
                    },
                ],
            }],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(pass_through_depth_state()),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let instance_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aegis-orbit-instances"),
        size: (initial_capacity as u64) * (std::mem::size_of::<OrbitInstance>() as u64),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    (pipeline, bgl, camera_buf, bind_group, instance_buf)
}

/// Build the orbit-trail pipeline + initial vertex buffer.
/// Returns `(pipeline, bgl, uniform_buf, bind_group, vertex_buf)`.
/// Plan 0004 M3.
fn build_orbit_trail_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    initial_capacity: u32,
) -> (
    wgpu::RenderPipeline,
    wgpu::BindGroupLayout,
    wgpu::Buffer,
    wgpu::BindGroup,
    wgpu::Buffer,
) {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aegis-orbit-trail-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let uniform_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("aegis-orbit-trail-uniform"),
        contents: bytemuck::bytes_of(&OrbitTrailUniform::default()),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("aegis-orbit-trail-bg"),
        layout: &bgl,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buf.as_entire_binding(),
        }],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aegis-orbit-trail-shader"),
        source: wgpu::ShaderSource::Wgsl(ORBIT_TRAIL_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aegis-orbit-trail-layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aegis-orbit-trail-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<OrbitTrailVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 12,
                        shader_location: 1,
                    },
                ],
            }],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            // LineList: pairs of consecutive vertices form a segment.
            // Switched from LineStrip so we can pack multiple
            // satellite trails into one buffer + one draw call.
            topology: wgpu::PrimitiveTopology::LineList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: Some(pass_through_depth_state()),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let vertex_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("aegis-orbit-trail-verts"),
        size: (initial_capacity as u64) * (std::mem::size_of::<OrbitTrailVertex>() as u64),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    (pipeline, bgl, uniform_buf, bind_group, vertex_buf)
}

/// `Depth32Float` is the depth attachment used by the main pass.
/// The format is hardcoded across the renderer (texture creation +
/// every pipeline's DepthStencilState); this constant makes the
/// shared value explicit.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Create the depth attachment texture + its view, sized to the
/// current swapchain. Recreated on every `resize` so the depth
/// buffer always matches the colour buffer.
fn create_depth_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("aegis-depth"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// Pass-through depth state — `Always` compare + no depth write —
/// for every pipeline *except* the building pass. With this state,
/// draw order continues to decide what's on top (same as before
/// the depth attachment landed in M1 of plan 0014), and the depth
/// buffer carries information about buildings only.
fn pass_through_depth_state() -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Always),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

/// Depth-tested + depth-writing state used by the building pass.
/// Standard `Less` + write so building fragments correctly occlude
/// each other.
fn building_depth_state() -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format: DEPTH_FORMAT,
        depth_write_enabled: Some(true),
        depth_compare: Some(wgpu::CompareFunction::Less),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

fn srgb8_to_linear_rgba(r: u8, g: u8, b: u8, a: u8) -> [f32; 4] {
    [
        srgb8_to_linear(r),
        srgb8_to_linear(g),
        srgb8_to_linear(b),
        // Alpha is straight-through — gamma applies to colour only.
        a as f32 / 255.0,
    ]
}

// `TILE_UNIFORM_SIZE` is `pub(crate)`-readable for future use but isn't
// referenced from the renderer directly — keep it next to the struct
// it describes so future bind-group-dynamic-offset code can pick it
// up without re-deriving.
#[allow(dead_code)]
const _: () = {
    let _ = TILE_UNIFORM_SIZE;
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb8_endpoints() {
        // Black and white must round-trip exactly so cap (and future
        // overlay) colours hit the intended endpoints on an sRGB
        // surface. Anything else is a regression in the conversion.
        assert_eq!(srgb8_to_linear(0), 0.0);
        assert!((srgb8_to_linear(255) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn srgb8_middle_gray_matches_iec_61966() {
        // 188 in sRGB is the classic "middle-gray" reference (a
        // perceptual-50%-grey card). It must convert to ~0.5034 in
        // linear-light per the IEC 61966-2-1 transfer function.
        let mid = srgb8_to_linear(188);
        assert!(
            (mid - 0.5034).abs() < 1e-3,
            "srgb8_to_linear(188) = {mid}, expected ≈ 0.5034"
        );
    }

    #[test]
    fn srgb8_to_linear_rgba_passes_alpha_straight_through() {
        // Alpha is never gamma-encoded — even though the rgb channels
        // get the sRGB transform, alpha is straight 0..1.
        let rgba = srgb8_to_linear_rgba(0, 0, 0, 128);
        assert_eq!(rgba[0], 0.0);
        assert!((rgba[3] - 128.0 / 255.0).abs() < 1e-6);
    }

    /// Uniform structs are mirrored on the WGSL side; their sizes
    /// must match each shader's `Uniforms` / `Camera` / `CapUniforms`
    /// struct or fragments read garbage. Per AGENTS.md §Testing
    /// rule 2: pin the sizes so the day/night extension (plan 0009
    /// M0) — and any future trailing-vec3 pad — can't silently
    /// inflate the layout (see `feedback_wgsl_struct_layout`).
    #[test]
    fn uniform_struct_sizes_match_wgsl() {
        assert_eq!(std::mem::size_of::<TileUniforms>(), 128);
        assert_eq!(std::mem::size_of::<VectorCameraUniform>(), 112);
        assert_eq!(std::mem::size_of::<CapUniform>(), 112);
        assert_eq!(std::mem::size_of::<EarthCameraUniform>(), 96);
        assert_eq!(std::mem::size_of::<AtmosphereUniform>(), 144);
        assert_eq!(std::mem::size_of::<StarfieldUniform>(), 48);
        assert_eq!(std::mem::size_of::<BuildingUniforms>(), 128);
        assert_eq!(
            std::mem::size_of::<crate::buildings::BuildingPerInstance>(),
            16
        );
        assert_eq!(std::mem::size_of::<crate::buildings::BuildingVertex>(), 32);
    }
}
