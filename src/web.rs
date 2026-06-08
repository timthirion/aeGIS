//! wasm-bindgen entry points for the browser build.
//!
//! Single-instance for the foundation phase — the embedder-API
//! multi-instance shape lands in M4 of plan 0001.
//!
//! `start(host_id)` finds a host element by id, appends a `<canvas>`
//! sized to the host's CSS box × device pixel ratio, attaches a `wgpu`
//! surface to it, and drives a `requestAnimationFrame` loop. A
//! `ResizeObserver` keeps the backing-store size in sync with the
//! host's CSS size; pointer + wheel listeners route drag-to-pan and
//! wheel-to-zoom into the renderer's `Camera`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::render::{make_instance, Renderer};

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

/// State shared between the rAF tick and the observer callbacks.
/// Held in `Cell` (not `RefCell`) so the observer never collides with
/// the mutable borrow held during a render.
struct Shared {
    pending_resize: Cell<Option<(u32, u32)>>,
    /// `(x, y)` of the most recent pointer event in **physical** pixels
    /// (CSS px × devicePixelRatio) so the camera operates in the same
    /// space as the wgpu surface.
    cursor_px: Cell<(f64, f64)>,
    dragging: Cell<bool>,
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
        self.renderer.drain_completed_fetches();
        self.renderer.ensure_visible_tiles();
        self.renderer.render();
    }
}

/// A listener kept alive for the lifetime of the widget. Drop removes
/// the registration so a hot-reloaded wasm module doesn't accumulate
/// duplicate handlers on the host element.
struct ListenerHandle {
    target: web_sys::EventTarget,
    event: &'static str,
    closure: Closure<dyn FnMut(web_sys::Event)>,
}

impl Drop for ListenerHandle {
    fn drop(&mut self) {
        let _ = self
            .target
            .remove_event_listener_with_callback(self.event, self.closure.as_ref().unchecked_ref());
    }
}

fn attach(
    target: &web_sys::EventTarget,
    event: &'static str,
    closure: Closure<dyn FnMut(web_sys::Event)>,
    listeners: &mut Vec<ListenerHandle>,
) -> Result<(), JsValue> {
    target.add_event_listener_with_callback(event, closure.as_ref().unchecked_ref())?;
    listeners.push(ListenerHandle {
        target: target.clone(),
        event,
        closure,
    });
    Ok(())
}

/// A live aeGIS renderer bound to a host element. Keep this handle alive
/// for the lifetime of the widget — dropping it stops the rAF loop and
/// detaches the resize observer + pointer listeners.
#[wasm_bindgen]
pub struct AegisInstance {
    _inner: Rc<RefCell<Inner>>,
    _raf: Rc<RefCell<Option<Closure<dyn FnMut()>>>>,
    _resize_observer: web_sys::ResizeObserver,
    _resize_cb: Closure<dyn FnMut()>,
    _listeners: Vec<ListenerHandle>,
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
        cursor_px: Cell::new((0.0, 0.0)),
        dragging: Cell::new(false),
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

    // Pointer + wheel listeners. Cursor coords come in as **CSS pixels**
    // relative to the canvas; multiply by devicePixelRatio to match the
    // physical-pixel space the camera works in.
    let mut listeners: Vec<ListenerHandle> = Vec::new();
    let canvas_target: web_sys::EventTarget = canvas.clone().into();

    fn cursor_from_event(
        e: &web_sys::MouseEvent,
        canvas: &web_sys::HtmlCanvasElement,
    ) -> (f64, f64) {
        let rect = canvas.get_bounding_client_rect();
        let dpr = web_window().device_pixel_ratio().max(1.0);
        (
            (e.client_x() as f64 - rect.left()) * dpr,
            (e.client_y() as f64 - rect.top()) * dpr,
        )
    }

    // pointerdown — start drag.
    attach(
        &canvas_target,
        "pointerdown",
        {
            let shared = shared.clone();
            let canvas = canvas.clone();
            Closure::wrap(Box::new(move |e: web_sys::Event| {
                if let Ok(m) = e.dyn_into::<web_sys::MouseEvent>() {
                    shared.cursor_px.set(cursor_from_event(&m, &canvas));
                    shared.dragging.set(true);
                }
            }) as Box<dyn FnMut(web_sys::Event)>)
        },
        &mut listeners,
    )?;

    // pointermove — pan while dragging.
    attach(
        &canvas_target,
        "pointermove",
        {
            let shared = shared.clone();
            let canvas = canvas.clone();
            let inner = inner.clone();
            Closure::wrap(Box::new(move |e: web_sys::Event| {
                if let Ok(m) = e.dyn_into::<web_sys::MouseEvent>() {
                    let new_cursor = cursor_from_event(&m, &canvas);
                    if shared.dragging.get() {
                        let prev = shared.cursor_px.get();
                        let dx = new_cursor.0 - prev.0;
                        let dy = new_cursor.1 - prev.1;
                        inner.borrow_mut().renderer.camera.pan(dx, dy);
                    }
                    shared.cursor_px.set(new_cursor);
                }
            }) as Box<dyn FnMut(web_sys::Event)>)
        },
        &mut listeners,
    )?;

    // pointerup / pointercancel / pointerleave — end drag.
    let make_release = || {
        let shared = shared.clone();
        Closure::wrap(Box::new(move |_e: web_sys::Event| {
            shared.dragging.set(false);
        }) as Box<dyn FnMut(web_sys::Event)>)
    };
    attach(&canvas_target, "pointerup", make_release(), &mut listeners)?;
    attach(
        &canvas_target,
        "pointercancel",
        make_release(),
        &mut listeners,
    )?;
    attach(
        &canvas_target,
        "pointerleave",
        make_release(),
        &mut listeners,
    )?;

    // wheel — zoom around the cursor. Browsers' WheelEvent.deltaY is
    // positive when scrolling **down** (towards the user), which we
    // map to "zoom out" by inverting the sign.
    attach(
        &canvas_target,
        "wheel",
        {
            let shared = shared.clone();
            let canvas = canvas.clone();
            let inner = inner.clone();
            Closure::wrap(Box::new(move |e: web_sys::Event| {
                if let Ok(w) = e.dyn_into::<web_sys::WheelEvent>() {
                    w.prevent_default(); // stop the page from scrolling
                    let cursor = cursor_from_event(w.as_ref(), &canvas);
                    shared.cursor_px.set(cursor);
                    let canvas_size = inner.borrow().renderer.size();
                    // 0.005 per pixel-delta gives a 1 zoom step per
                    // ~200 px of trackpad pan — comfortable.
                    let zoom_delta = -w.delta_y() * 0.005;
                    inner
                        .borrow_mut()
                        .renderer
                        .camera
                        .zoom_at(zoom_delta, cursor, canvas_size);
                }
            }) as Box<dyn FnMut(web_sys::Event)>)
        },
        &mut listeners,
    )?;

    Ok(AegisInstance {
        _inner: inner,
        _raf: raf,
        _resize_observer: resize_observer,
        _resize_cb: resize_cb,
        _listeners: listeners,
    })
}
