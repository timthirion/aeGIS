//! Renderer core — owns the wgpu `Device`, `Queue`, `Surface`, and the
//! pipelines. Platform-agnostic; the native and web entry points
//! construct one and call `render()` per frame.
//!
//! M0 was a single clear pass. M1.5 ("first tile") adds the textured-
//! quad tile pass on top: when [`Renderer::set_tile`] has been called,
//! `render()` draws the tile; otherwise the M0 gradient still shows as
//! a loading/fallback state.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

const CLEAR_SHADER: &str = include_str!("shaders/clear.wgsl");
const TILE_SHADER: &str = include_str!("shaders/tile.wgsl");

/// Construct a wgpu instance suitable for both native and browser targets.
pub fn make_instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    })
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct TileUniforms {
    // (sx, sy): aspect-correction multipliers in NDC. Padded to 16 B.
    scale: [f32; 2],
    _padding: [f32; 2],
}

/// A raster tile that's been uploaded to the GPU and is ready to render.
struct TileBinding {
    bind_group: wgpu::BindGroup,
    aspect: f32, // tile_w / tile_h — square tiles are 1.0
}

/// The renderer's per-window/canvas state. One `Renderer` per surface;
/// keep alive for the lifetime of the surface it owns.
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    clear_pipeline: wgpu::RenderPipeline,

    tile_pipeline: wgpu::RenderPipeline,
    tile_bgl: wgpu::BindGroupLayout,
    tile_sampler: wgpu::Sampler,
    tile_uniforms_buf: wgpu::Buffer,
    tile: Option<TileBinding>,
}

impl Renderer {
    /// Initialise a renderer against the given surface.
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
        // Prefer an sRGB surface so PNG-sourced tile pixels (sRGB-
        // encoded per spec) render with correct gamma end-to-end: the
        // GPU's automatic sRGB→linear on texture read + linear→sRGB on
        // surface write cancel out. The M0 gradient was hand-tuned in
        // linear-ish space; on an sRGB surface it reads slightly more
        // saturated, which is fine for a fallback / loading state.
        let format = surface_caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(surface_caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode: surface_caps.present_modes[0],
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let clear_pipeline = build_clear_pipeline(&device, format);

        let tile_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("aegis-tile-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
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

        let tile_uniforms_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("aegis-tile-uniforms"),
            contents: bytemuck::bytes_of(&TileUniforms::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        Renderer {
            surface,
            device,
            queue,
            config,
            clear_pipeline,
            tile_pipeline,
            tile_bgl,
            tile_sampler,
            tile_uniforms_buf,
            tile: None,
        }
    }

    /// Surface dimensions in physical pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Reconfigure the surface to a new drawable size and refresh the
    /// tile-aspect uniform.
    pub fn resize(&mut self, width: u32, height: u32) {
        let (w, h) = (width.max(1), height.max(1));
        if w == self.config.width && h == self.config.height {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
        self.refresh_tile_uniforms();
    }

    /// Upload an RGBA raster tile (e.g. 256×256 from PNG decode) and
    /// bind it as the current tile. Subsequent `render()` calls draw it
    /// in place of the fallback clear-gradient.
    pub fn set_tile(&mut self, width: u32, height: u32, rgba: &[u8]) {
        assert_eq!(
            rgba.len(),
            (width * height * 4) as usize,
            "set_tile: rgba.len()={} doesn't match {width}×{height}×4",
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
            // sRGB so PNG-sourced bytes are decoded to linear on sample;
            // the surface (also sRGB) re-encodes on present.
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

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aegis-tile-bg"),
            layout: &self.tile_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.tile_uniforms_buf.as_entire_binding(),
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

        self.tile = Some(TileBinding {
            bind_group,
            aspect: width as f32 / height as f32,
        });
        self.refresh_tile_uniforms();
        log::info!("set_tile: uploaded {width}×{height} RGBA tile");
    }

    fn refresh_tile_uniforms(&self) {
        let Some(tile) = &self.tile else { return };
        let (cw, ch) = (self.config.width as f32, self.config.height as f32);
        let canvas_aspect = cw / ch;
        let scale = if canvas_aspect >= tile.aspect {
            // Canvas wider than the tile: shrink X (pillarbox the
            // sides).
            [tile.aspect / canvas_aspect, 1.0]
        } else {
            // Canvas taller than the tile: shrink Y (letterbox top +
            // bottom).
            [1.0, canvas_aspect / tile.aspect]
        };
        let u = TileUniforms {
            scale,
            _padding: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.tile_uniforms_buf, 0, bytemuck::bytes_of(&u));
    }

    /// Draw one frame.
    pub fn render(&self) {
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
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
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
                        // Background letterbox colour: a dark
                        // near-black (matches the UI chrome in
                        // index.html). On the sRGB surface the GPU
                        // re-encodes the linear value we write, so
                        // these are linear-space coordinates for the
                        // sRGB display target #0b0e12.
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

            if let Some(tile) = &self.tile {
                pass.set_pipeline(&self.tile_pipeline);
                pass.set_bind_group(0, &tile.bind_group, &[]);
                pass.draw(0..3, 0..1);
            } else {
                // Fallback / loading state — the M0 gradient.
                pass.set_pipeline(&self.clear_pipeline);
                pass.draw(0..3, 0..1);
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
    }
}

fn build_clear_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("aegis-clear-shader"),
        source: wgpu::ShaderSource::Wgsl(CLEAR_SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("aegis-clear-layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("aegis-clear-pipeline"),
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
                blend: None,
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
                blend: None,
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
