//! wasm-bindgen entry points for the browser build.
//!
//! M0: `start(host_id)` finds a host element by id, appends a `<canvas>`
//! sized to the host's CSS box × device pixel ratio, attaches a `wgpu`
//! surface to it, and drives a `requestAnimationFrame` loop. A
//! `ResizeObserver` keeps the backing-store size in sync with the host's
//! CSS size.
//!
//! Single-instance for M0 — the embedder-API multi-instance shape lands
//! in M4 ([[project-direction]] Phase 0 plan 0001).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::render::{make_instance, Renderer};
use crate::tile;

#[wasm_bindgen(start)]
pub fn on_module_load() {
    console_error_panic_hook::set_once();
    // Best-effort log init — duplicate calls are a no-op.
    let _ = console_log::init_with_level(log::Level::Info);
}

#[wasm_bindgen]
pub fn version() -> String {
    crate::version::VERSION.to_string()
}

fn web_window() -> web_sys::Window {
    web_sys::window().expect("no global window")
}

fn request_animation_frame(cb: &Closure<dyn FnMut()>) {
    web_window()
        .request_animation_frame(cb.as_ref().unchecked_ref())
        .expect("requestAnimationFrame failed");
}

/// Backing-store size for the canvas: the host's CSS size × devicePixelRatio.
/// Clamped to `(1, 1)` minimum so wgpu never sees a zero-sized surface.
fn backing_size(host: &web_sys::Element) -> (u32, u32) {
    let dpr = web_window().device_pixel_ratio().max(1.0);
    let w = (host.client_width().max(1) as f64 * dpr).round() as u32;
    let h = (host.client_height().max(1) as f64 * dpr).round() as u32;
    (w.max(1), h.max(1))
}

/// State shared between the rAF tick and the resize-observer callback.
/// Held in `Cell` (not `RefCell`) so the observer never collides with the
/// mutable borrow held during a render.
struct Shared {
    pending_resize: Cell<Option<(u32, u32)>>,
}

struct Inner {
    renderer: Renderer,
    canvas: web_sys::HtmlCanvasElement,
    shared: Rc<Shared>,
}

impl Inner {
    fn tick(&mut self) {
        if let Some((w, h)) = self.shared.pending_resize.take() {
            self.canvas.set_width(w);
            self.canvas.set_height(h);
            self.renderer.resize(w, h);
        }
        self.renderer.render();
    }
}

/// A live aeGIS renderer bound to a host element. Keep this handle alive
/// for the lifetime of the widget — dropping it stops the rAF loop and
/// detaches the resize observer.
#[wasm_bindgen]
pub struct AegisInstance {
    _inner: Rc<RefCell<Inner>>,
    _raf: Rc<RefCell<Option<Closure<dyn FnMut()>>>>,
    _resize_observer: web_sys::ResizeObserver,
    _resize_cb: Closure<dyn FnMut()>,
}

/// Attach an aeGIS renderer to the element with the given id. The host
/// element should have non-zero CSS dimensions; the canvas backs onto it.
#[wasm_bindgen]
pub async fn start(host_id: String) -> Result<AegisInstance, JsValue> {
    let document = web_window()
        .document()
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let host = document
        .get_element_by_id(&host_id)
        .ok_or_else(|| JsValue::from_str(&format!("no element #{host_id}")))?;

    let canvas: web_sys::HtmlCanvasElement = document
        .create_element("canvas")?
        .dyn_into()
        .map_err(|_| JsValue::from_str("canvas dyn_into failed"))?;
    // Sizing is the embedder's CSS responsibility; we just append. See
    // the `#aegis-host canvas` rule in `index.html` for the default.
    host.append_child(&canvas)?;

    let (w, h) = backing_size(&host);
    canvas.set_width(w);
    canvas.set_height(h);

    let instance = make_instance();
    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
        .map_err(|e| JsValue::from_str(&format!("create_surface: {e:?}")))?;
    let renderer = Renderer::new(instance, surface, w, h).await;

    log::info!(
        "aegis {} attached to #{host_id} ({w}x{h})",
        crate::version::VERSION
    );

    let shared = Rc::new(Shared {
        pending_resize: Cell::new(None),
    });
    let inner = Rc::new(RefCell::new(Inner {
        renderer,
        canvas: canvas.clone(),
        shared: shared.clone(),
    }));

    // rAF loop — self-rescheduling closure.
    let raf: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));
    {
        let raf2 = raf.clone();
        let inner2 = inner.clone();
        *raf.borrow_mut() = Some(Closure::wrap(Box::new(move || {
            inner2.borrow_mut().tick();
            if let Some(cb) = raf2.borrow().as_ref() {
                request_animation_frame(cb);
            }
        }) as Box<dyn FnMut()>));
    }
    request_animation_frame(raf.borrow().as_ref().unwrap());

    // ResizeObserver — kicks pending_resize on the next tick.
    let resize_cb = {
        let shared = shared.clone();
        let host_el: web_sys::Element = host.clone();
        Closure::wrap(Box::new(move || {
            shared.pending_resize.set(Some(backing_size(&host_el)));
        }) as Box<dyn FnMut()>)
    };
    let resize_observer = web_sys::ResizeObserver::new(resize_cb.as_ref().unchecked_ref())?;
    resize_observer.observe(&host);

    // Plan 0001 "first tile" — kick off a single OSM tile fetch
    // centred on Chicago at zoom 10. The fetch is fire-and-forget;
    // the callback runs on the JS event loop when bytes arrive, then
    // borrows `inner` (the rAF tick can't be borrowing concurrently
    // because JS callbacks run sequentially on the main thread).
    let (lon, lat) = tile::CHICAGO_LONLAT;
    let tile_id = tile::TileId::from_lonlat(10, lon, lat);
    let url = tile_id.osm_url();
    log::info!("fetching startup tile: {url}");
    let inner_for_fetch = inner.clone();
    tile::fetch_tile_async(&url, move |result| match result {
        Ok(decoded) => {
            inner_for_fetch.borrow_mut().renderer.set_tile(
                decoded.width,
                decoded.height,
                &decoded.rgba,
            );
        }
        Err(e) => log::warn!("startup tile fetch failed ({e}); showing fallback gradient"),
    });

    Ok(AegisInstance {
        _inner: inner,
        _raf: raf,
        _resize_observer: resize_observer,
        _resize_cb: resize_cb,
    })
}
