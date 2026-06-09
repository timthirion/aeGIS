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

use crate::camera::Camera;
use crate::tile::{self, DecodedTile, TileId, CHICAGO_LONLAT};
use crate::vector::VectorLayer;

const TILE_SHADER: &str = include_str!("shaders/tile.wgsl");
const VECTOR_SHADER: &str = include_str!("shaders/vector.wgsl");
const CAPS_SHADER: &str = include_str!("shaders/caps.wgsl");
const EARTH_SHADER: &str = include_str!("shaders/earth.wgsl");

/// Blue Marble equirectangular Earth imagery, embedded at compile time
/// so it ships with the wasm and native binaries alike. 1024×512 PNG,
/// ~450 KB. See `data/blue-marble/ATTRIBUTION.md`.
const EARTH_PNG_BYTES: &[u8] = include_bytes!("../data/blue-marble/earth_1024x512.png");

/// Vertex count for the full Earth sphere — `LAT_BANDS × LON_SEGMENTS`
/// quads × 6 verts/quad. Mirrors the constants in `earth.wgsl`.
const EARTH_DRAW_VERTS: u32 = 64 * 128 * 6;

/// Triangles per polar cap. Matches `RING_VERTS` in `caps.wgsl`.
const CAP_RING_VERTS: u32 = 64;
/// Vertex count per cap = 3 verts × `CAP_RING_VERTS` triangles.
const CAP_DRAW_VERTS: u32 = 3 * CAP_RING_VERTS;

const TILE_UNIFORM_SIZE: u64 = std::mem::size_of::<TileUniforms>() as u64;

/// Construct a wgpu instance suitable for both native and browser targets.
pub fn make_instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    })
}

/// Per-tile uniform consumed by `tile.wgsl`. Matches the WGSL
/// `Uniforms` struct byte-for-byte (6 × `vec4` = 96 bytes):
/// - rows 0–3: view-projection matrix (column-major)
/// - row 4: camera position (xyz) + 1 pad
/// - row 5: tile's world rect (xmin, ymin, xmax, ymax)
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct TileUniforms {
    view_proj: [f32; 16],
    camera_pos: [f32; 3],
    _pad0: f32,
    world_rect: [f32; 4],
}

/// Per-frame camera uniform consumed by `vector.wgsl`. Matches the
/// WGSL `Camera` struct byte-for-byte (5 × `vec4` = 80 bytes):
/// - rows 0–3: view-projection matrix (column-major)
/// - row 4: camera position (xyz) + 1 pad
/// - row 5: color (rgba)
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct VectorCameraUniform {
    view_proj: [f32; 16],
    position: [f32; 3],
    _pad0: f32,
    color: [f32; 4],
}

/// Per-cap uniform consumed by `caps.wgsl`. Same 80-byte layout as
/// `VectorCameraUniform` with `position` reused for camera_pos and
/// the trailing f32 carrying `pole_sign` (+1 north, −1 south) instead
/// of pad.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct CapUniform {
    view_proj: [f32; 16],
    camera_pos: [f32; 3],
    pole_sign: f32,
    color: [f32; 4],
}

/// Per-frame camera uniform consumed by `earth.wgsl`. 80 bytes —
/// view_proj, camera_pos (used for the back-hemisphere discard), pad.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct EarthCameraUniform {
    view_proj: [f32; 16],
    position: [f32; 3],
    _pad0: f32,
}

/// A vector-layer that's been uploaded as a GPU vertex buffer, ready
/// to render as a LineList.
struct VectorBinding {
    vertex_buf: wgpu::Buffer,
    vertex_count: u32,
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

    tile_pipeline: wgpu::RenderPipeline,
    tile_bgl: wgpu::BindGroupLayout,
    tile_sampler: wgpu::Sampler,

    vector_pipeline: wgpu::RenderPipeline,
    vector_camera_buf: wgpu::Buffer,
    vector_bind_group: wgpu::BindGroup,
    vector: Option<VectorBinding>,

    /// Polar-cap pipeline + one uniform buffer per cap. The shader is
    /// pure-procedural (no vertex buffer), so each draw needs only its
    /// own uniform bind-group to differentiate north (blue ocean) from
    /// south (white ice sheet).
    cap_pipeline: wgpu::RenderPipeline,
    north_cap_buf: wgpu::Buffer,
    north_cap_bind_group: wgpu::BindGroup,
    south_cap_buf: wgpu::Buffer,
    south_cap_bind_group: wgpu::BindGroup,

    /// Earth-texture pipeline. Procedurally-tessellated unit sphere
    /// sampled from a bundled Blue Marble equirectangular PNG. Drawn
    /// before tiles so loaded tiles overdraw the texture in their
    /// region; covers polar latitudes the tile pyramid can't reach.
    earth_pipeline: wgpu::RenderPipeline,
    earth_camera_buf: wgpu::Buffer,
    earth_bind_group: wgpu::BindGroup,
    /// Kept alive so the bind-group's texture view + sampler stay
    /// valid for the renderer's lifetime — same pattern as
    /// `tile_sampler` and `TileBinding._texture`.
    _earth_texture: wgpu::Texture,
    _earth_sampler: wgpu::Sampler,

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

    /// Camera state. Public for direct mutation by input handlers
    /// (`renderer.camera.pan(...)`, `renderer.camera.zoom_at(...)`).
    pub camera: Camera,
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

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("aegis-device"),
                required_features: wgpu::Features::empty(),
                required_limits: if cfg!(target_arch = "wasm32") {
                    wgpu::Limits::downlevel_webgl2_defaults()
                } else {
                    wgpu::Limits::default()
                },
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
        let (earth_pipeline, earth_camera_buf, earth_bind_group, earth_texture, earth_sampler) =
            build_earth_resources(&device, &queue, format);

        let (completed_tx, completed_rx) = mpsc::channel();

        Renderer {
            surface,
            device,
            queue,
            config,
            view_format,
            tile_pipeline,
            tile_bgl,
            tile_sampler,
            vector_pipeline,
            vector_camera_buf,
            vector_bind_group,
            vector: None,
            cap_pipeline,
            north_cap_buf,
            north_cap_bind_group,
            south_cap_buf,
            south_cap_bind_group,
            earth_pipeline,
            earth_camera_buf,
            earth_bind_group,
            _earth_texture: earth_texture,
            _earth_sampler: earth_sampler,
            tiles: HashMap::new(),
            requested: HashSet::new(),
            failed: HashSet::new(),
            completed_tx,
            completed_rx,
            // Default view: a partly-globey zoom centred between
            // the Americas so the headline globe view is the first
            // thing the user sees. They can scroll in to land at
            // Chicago / any flat-Mercator view.
            camera: Camera::new(CHICAGO_LONLAT.0, 30.0, 1.5),
        }
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
        let url = id.tile_url();
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
        let url = id.tile_url();
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
        let view_proj = self.camera.view_projection_matrix(canvas);
        let camera_pos = self.camera.camera_3d_position(canvas);

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
        let mut draws: Vec<(&TileId, [f32; 4], &TileBinding)> = self
            .tiles
            .iter()
            .filter(|(id, _)| id.z <= current_z)
            .map(|(id, binding)| (id, id.world_rect(), binding))
            .collect();
        // Coarse-first: finer tiles overdraw their parents.
        draws.sort_by_key(|(id, _, _)| id.z);

        for (_, world_rect, binding) in &draws {
            let u = TileUniforms {
                view_proj,
                camera_pos,
                _pad0: 0.0,
                world_rect: *world_rect,
            };
            self.queue
                .write_buffer(&binding.uniform_buf, 0, bytemuck::bytes_of(&u));
        }

        // Per-frame vector-camera uniform. Single 80-byte upload.
        let vector_camera = VectorCameraUniform {
            view_proj,
            position: camera_pos,
            _pad0: 0.0,
            // Country-outline overlay colour: coral-orange that reads
            // against OSM's brown/beige basemap. Alpha kept moderate
            // so the basemap shows faintly under the lines.
            color: [0.95, 0.42, 0.22, 0.85],
        };
        self.queue.write_buffer(
            &self.vector_camera_buf,
            0,
            bytemuck::bytes_of(&vector_camera),
        );

        // Per-frame cap uniforms. Same view_proj + camera_pos as the
        // other passes; the per-cap `pole_sign` and `color` are baked
        // into each buffer.
        //
        // Cap colours are the user's hand-picked **sRGB** values
        // (north = pale Arctic blue, south = warm Antarctic ice).
        // `srgb8_to_linear_rgba` round-trips them through the sRGB
        // surface so what lands on screen is exactly the picked
        // triple — no shading, no lighting, no compositing tricks
        // alter them between the uniform and the framebuffer.
        let north_cap = CapUniform {
            view_proj,
            camera_pos,
            pole_sign: 1.0,
            color: srgb8_to_linear_rgba(170, 206, 212, 255),
        };
        let south_cap = CapUniform {
            view_proj,
            camera_pos,
            pole_sign: -1.0,
            color: srgb8_to_linear_rgba(246, 239, 229, 255),
        };
        self.queue
            .write_buffer(&self.north_cap_buf, 0, bytemuck::bytes_of(&north_cap));
        self.queue
            .write_buffer(&self.south_cap_buf, 0, bytemuck::bytes_of(&south_cap));

        // Earth-texture camera uniform — view_proj + position (used
        // for the back-hemisphere discard in earth.wgsl).
        let earth_camera = EarthCameraUniform {
            view_proj,
            position: camera_pos,
            _pad0: 0.0,
        };
        self.queue
            .write_buffer(&self.earth_camera_buf, 0, bytemuck::bytes_of(&earth_camera));

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
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Earth texture first — covers the full sphere with the
            // bundled Blue Marble imagery. Loaded tiles will overdraw
            // it in their region; the texture remains the visible
            // surface anywhere tiles haven't loaded yet (cold cache,
            // panning ahead of fetches, polar latitudes the Mercator
            // pyramid can't reach, the back hemisphere — discarded).
            pass.set_pipeline(&self.earth_pipeline);
            pass.set_bind_group(0, &self.earth_bind_group, &[]);
            pass.draw(0..EARTH_DRAW_VERTS, 0..1);

            if !draws.is_empty() {
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

            // Polar caps — fill the spherical band the Mercator tile
            // pyramid can't cover (|lat| > 85.051°). Drawn after the
            // tiles so the cap colour overrides any background pixels
            // that show through near the poles; the per-fragment
            // visibility discard handles the back hemisphere.
            pass.set_pipeline(&self.cap_pipeline);
            pass.set_bind_group(0, &self.north_cap_bind_group, &[]);
            pass.draw(0..CAP_DRAW_VERTS, 0..1);
            pass.set_bind_group(0, &self.south_cap_bind_group, &[]);
            pass.draw(0..CAP_DRAW_VERTS, 0..1);

            // Vector overlay on top of the basemap.
            if let Some(vector) = &self.vector {
                pass.set_pipeline(&self.vector_pipeline);
                pass.set_bind_group(0, &self.vector_bind_group, &[]);
                pass.set_vertex_buffer(0, vector.vertex_buf.slice(..));
                pass.draw(0..vector.vertex_count, 0..1);
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
        depth_stencil: None,
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
        depth_stencil: None,
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
        depth_stencil: None,
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
fn build_earth_resources(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
) -> (
    wgpu::RenderPipeline,
    wgpu::Buffer,
    wgpu::BindGroup,
    wgpu::Texture,
    wgpu::Sampler,
) {
    let decoded = tile::decode_png(EARTH_PNG_BYTES)
        .expect("bundled Blue Marble PNG failed to decode — binary is corrupt");
    log::info!(
        "earth texture: decoded {}×{} from bundled PNG",
        decoded.width,
        decoded.height
    );

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("aegis-earth-texture"),
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
        label: Some("aegis-earth-sampler"),
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

    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("aegis-earth-bgl"),
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
        ],
    });
    let camera_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("aegis-earth-camera"),
        contents: bytemuck::bytes_of(&EarthCameraUniform::default()),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("aegis-earth-bg"),
        layout: &bgl,
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
        ],
    });

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aegis-earth-shader"),
        source: wgpu::ShaderSource::Wgsl(EARTH_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aegis-earth-layout"),
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aegis-earth-pipeline"),
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
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    // `view` lives only as long as `bind_group` references it; the
    // texture + sampler must outlive the bind group, so we hand them
    // back to the caller for storage on `Renderer`.
    (pipeline, camera_buf, bind_group, texture, sampler)
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
}
