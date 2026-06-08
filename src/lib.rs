//! aeGIS — open-source GIS in Rust, native + WebGPU.
//!
//! Owns the renderer, the layer model, the CRS subsystem, and the WGSL
//! pipelines. Thin native (winit) and web (canvas + `wasm-bindgen`) entry
//! points drive it.
//!
//! See `AGENTS.md` and `plans/ROADMAP.md` for the project's direction.

pub mod crs;
pub mod render;
pub mod tile;
pub mod version;

#[cfg(target_arch = "wasm32")]
pub mod web;

// ---------------------------------------------------------------------------
// Native: a single winit window + event loop driving one Renderer.
// ---------------------------------------------------------------------------

/// Open a native window and run the renderer until the user closes it
/// (or presses Escape).
#[cfg(not(target_arch = "wasm32"))]
pub fn run() {
    use std::sync::Arc;
    use winit::{
        event::{ElementState, Event, KeyEvent, WindowEvent},
        event_loop::EventLoop,
        keyboard::{KeyCode, PhysicalKey},
        window::WindowBuilder,
    };

    let event_loop = EventLoop::new().expect("failed to create event loop");
    let window = Arc::new(
        WindowBuilder::new()
            .with_title(format!("aegis {}", version::VERSION))
            .build(&event_loop)
            .expect("failed to create window"),
    );
    let size = window.inner_size();

    let instance = render::make_instance();
    let surface = instance
        .create_surface(window.clone())
        .expect("failed to create surface");
    let mut renderer = pollster::block_on(render::Renderer::new(
        instance,
        surface,
        size.width.max(1),
        size.height.max(1),
    ));

    event_loop
        .run(move |event, elwt| {
            if let Event::WindowEvent { window_id, event } = event {
                if window_id != window.id() {
                    return;
                }
                match event {
                    WindowEvent::CloseRequested
                    | WindowEvent::KeyboardInput {
                        event:
                            KeyEvent {
                                state: ElementState::Pressed,
                                physical_key: PhysicalKey::Code(KeyCode::Escape),
                                ..
                            },
                        ..
                    } => elwt.exit(),
                    WindowEvent::Resized(s) => renderer.resize(s.width, s.height),
                    WindowEvent::RedrawRequested => {
                        renderer.render();
                        window.request_redraw();
                    }
                    _ => {}
                }
            }
        })
        .expect("event loop error");
}
