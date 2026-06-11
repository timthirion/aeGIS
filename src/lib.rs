//! aeGIS — open-source GIS in Rust, native + WebGPU.
//!
//! Owns the renderer, the layer model, the CRS subsystem, and the WGSL
//! pipelines. Thin native (winit) and web (canvas + `wasm-bindgen`) entry
//! points drive it.
//!
//! See `AGENTS.md` and `plans/ROADMAP.md` for the project's direction.

pub mod body;
pub mod camera;
pub mod clock;
pub mod crs;
pub mod flyto;
pub mod net;
pub mod orbit;
pub mod render;
pub mod search;
pub mod sun;
pub mod tile;
pub mod vector;
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
    use crate::{orbit, render, vector};
    use std::sync::Arc;
    use winit::{
        event::{ElementState, Event, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent},
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
    // The renderer's `ensure_visible_tiles` fires the initial fetches
    // automatically on the first frame — no manual prefetch needed.

    // Load the bundled Natural Earth countries overlay (best-effort —
    // run from any working directory the file might not be present).
    let geojson_path = "data/natural-earth/countries.geojson";
    match std::fs::read_to_string(geojson_path) {
        Ok(source) => match vector::load_geojson_lines(&source) {
            Ok(layer) => {
                log::info!(
                    "loaded {} ({} segments)",
                    geojson_path,
                    layer.segment_count()
                );
                renderer.set_vector_layer(&layer);
            }
            Err(e) => log::warn!("parse {geojson_path}: {e}"),
        },
        Err(e) => log::warn!("read {geojson_path}: {e} — running without vector overlay"),
    }

    // Load the bundled ISS TLE fixture (plan 0004 M0 / M1). The
    // native build doesn't reach out to Celestrak on startup — the
    // fixture is the only satellite shown on `cargo run` unless the
    // caller invokes `Renderer::load_satellites` with fresher TLE
    // text themselves.
    // Pre-load the bundled ISS TLE fixture so the Stations category
    // has data immediately when the user toggles it on. Nothing is
    // selected by default — the satellite-list panel surfaces only
    // when the user enables at least one category.
    const ISS_FIXTURE: &str = include_str!("../data/orbits/iss-fixture.txt");
    renderer.load_satellites(orbit::Category::Stations, ISS_FIXTURE);

    let mut cursor_px: (f64, f64) = (0.0, 0.0);
    let mut dragging = false;
    // Monotonic clock for the fly-to animation. `Instant::now()` is
    // the same source the renderer was already implicitly using via
    // `std::thread::spawn` timestamps in the tile fetcher.
    let startup = std::time::Instant::now();

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
                    WindowEvent::KeyboardInput {
                        event:
                            KeyEvent {
                                state: ElementState::Pressed,
                                physical_key: PhysicalKey::Code(KeyCode::KeyB),
                                ..
                            },
                        ..
                    } => {
                        // 'B' for basemap: Map ↔ Satellite.
                        let next = match renderer.basemap_mode() {
                            render::BasemapMode::Map => render::BasemapMode::Satellite,
                            render::BasemapMode::Satellite => render::BasemapMode::Map,
                        };
                        renderer.set_basemap_mode(next);
                    }
                    WindowEvent::Resized(s) => renderer.resize(s.width, s.height),
                    WindowEvent::MouseInput {
                        state: btn_state,
                        button: MouseButton::Left,
                        ..
                    } => {
                        dragging = btn_state == ElementState::Pressed;
                    }
                    WindowEvent::CursorMoved { position, .. } => {
                        let new_cursor = (position.x, position.y);
                        if dragging {
                            let dx = new_cursor.0 - cursor_px.0;
                            let dy = new_cursor.1 - cursor_px.1;
                            let canvas = renderer.size();
                            renderer.user_pan(dx, dy, canvas);
                        }
                        cursor_px = new_cursor;
                    }
                    WindowEvent::MouseWheel { delta, .. } => {
                        let zoom_delta = match delta {
                            // Mouse wheel: half a zoom per click.
                            MouseScrollDelta::LineDelta(_, y) => y as f64 * 0.5,
                            // Trackpad: 0.01 zoom/px → ~1 zoom per
                            // 100 px of vertical pan. Roughly 2x the
                            // earlier 0.005 to match the web target's
                            // bumped step + the user-reported
                            // "needs to be more responsive" feel.
                            MouseScrollDelta::PixelDelta(p) => p.y * 0.01,
                        };
                        renderer.user_zoom_at(zoom_delta, cursor_px, renderer.size());
                    }
                    WindowEvent::RedrawRequested => {
                        renderer.drain_completed_fetches();
                        renderer.drain_sat_completed_fetches();
                        let now = startup.elapsed().as_secs_f64();
                        renderer.tick_fly_to(now);
                        renderer.tick_orbit(now);
                        renderer.ensure_visible_tiles();
                        renderer.ensure_visible_sat_tiles();
                        renderer.render();
                        window.request_redraw();
                    }
                    _ => {}
                }
            }
        })
        .expect("event loop error");
}
