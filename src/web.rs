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

use crate::render::{make_instance, BasemapMode, Renderer};
use crate::search::{geocode_async, parse_coord, GeocoderClient, ResultKind, SearchResult};

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

/// Fetch a text resource (used for the GeoJSON overlay). Browser sets
/// `User-Agent`; we don't need to.
async fn fetch_text(url: &str) -> Result<String, String> {
    use wasm_bindgen_futures::JsFuture;
    let resp_value = JsFuture::from(web_window().fetch_with_str(url))
        .await
        .map_err(|e| format!("fetch: {e:?}"))?;
    let resp: web_sys::Response = resp_value
        .dyn_into()
        .map_err(|e| format!("Response cast: {e:?}"))?;
    if !resp.ok() {
        return Err(format!("HTTP {} for {url}", resp.status()));
    }
    let text_promise = resp.text().map_err(|e| format!("text(): {e:?}"))?;
    let text_value = JsFuture::from(text_promise)
        .await
        .map_err(|e| format!("text await: {e:?}"))?;
    text_value
        .as_string()
        .ok_or_else(|| "text was not a string".to_string())
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
        self.renderer.drain_sat_completed_fetches();
        // Monotonic seconds since the page loaded. `performance.now()`
        // returns ms — divide for the seconds the fly-to sampler wants.
        let now = web_window().performance().map_or(0.0, |p| p.now() / 1000.0);
        self.renderer.tick_fly_to(now);
        self.renderer.ensure_visible_tiles();
        self.renderer.ensure_visible_sat_tiles();
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
    inner: Rc<RefCell<Inner>>,
    _raf: Rc<RefCell<Option<Closure<dyn FnMut()>>>>,
    _resize_observer: web_sys::ResizeObserver,
    _resize_cb: Closure<dyn FnMut()>,
    _listeners: Vec<ListenerHandle>,
    /// Kept alive for the lifetime of the widget so the search-bar
    /// listeners stay registered. `None` when the host page doesn't
    /// include `#aegis-search` — the renderer works fine without
    /// the search UI.
    _search: Option<SearchHandles>,
}

#[wasm_bindgen]
impl AegisInstance {
    /// Switch the basemap. Pass `"map"` for Carto Voyager or
    /// `"satellite"` for NASA Blue Marble. Unknown values are
    /// silently ignored — the toggle button in the host HTML drives
    /// this, so a typo there should be visible as "click does nothing"
    /// rather than a thrown error.
    #[wasm_bindgen(js_name = setBasemap)]
    pub fn set_basemap(&self, mode: &str) {
        let parsed = match mode {
            "map" => Some(BasemapMode::Map),
            "satellite" => Some(BasemapMode::Satellite),
            _ => None,
        };
        if let Some(m) = parsed {
            self.inner.borrow_mut().renderer.set_basemap_mode(m);
        }
    }

    /// Returns the current basemap as `"map"` or `"satellite"`.
    #[wasm_bindgen(js_name = basemap)]
    pub fn basemap(&self) -> String {
        match self.inner.borrow().renderer.basemap_mode() {
            BasemapMode::Map => "map".into(),
            BasemapMode::Satellite => "satellite".into(),
        }
    }
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

    // Fetch the Natural Earth countries overlay async. The fetch
    // runs concurrently with the first few frames of tile loading;
    // when it lands, the overlay appears on top of whatever tiles
    // are already up.
    {
        let inner_for_vector = inner.clone();
        wasm_bindgen_futures::spawn_local(async move {
            match fetch_text("./data/natural-earth/countries.geojson").await {
                Ok(source) => match crate::vector::load_geojson_lines(&source) {
                    Ok(layer) => {
                        log::info!(
                            "loaded countries.geojson ({} segments)",
                            layer.segment_count()
                        );
                        inner_for_vector
                            .borrow_mut()
                            .renderer
                            .set_vector_layer(&layer);
                    }
                    Err(e) => log::warn!("parse countries.geojson: {e}"),
                },
                Err(e) => log::warn!("fetch countries.geojson: {e}"),
            }
        });
    }

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

    // pointerdown — start drag. Capture the pointer so subsequent
    // `pointermove` events keep landing on the canvas even after the
    // cursor leaves it. Without this, the user drags out past the
    // canvas edge and the camera stops following — the classic
    // "pan stops at the edge" bug.
    attach(
        &canvas_target,
        "pointerdown",
        {
            let shared = shared.clone();
            let canvas = canvas.clone();
            Closure::wrap(Box::new(move |e: web_sys::Event| {
                if let Ok(p) = e.dyn_into::<web_sys::PointerEvent>() {
                    shared.cursor_px.set(cursor_from_event(p.as_ref(), &canvas));
                    shared.dragging.set(true);
                    let _ = canvas.set_pointer_capture(p.pointer_id());
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
                        let mut inner_mut = inner.borrow_mut();
                        let canvas_size = inner_mut.renderer.size();
                        inner_mut.renderer.user_pan(dx, dy, canvas_size);
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

    // wheel — zoom around the cursor. Browsers' `WheelEvent.deltaY`
    // is positive when scrolling down (toward the user) → we invert to
    // map "scroll down" to "zoom out". Critical: `deltaY` is in units
    // determined by `deltaMode` — PIXEL (0), LINE (1), or PAGE (2).
    //
    // Mouse wheels deliver LINE-mode events at ~1 click per call —
    // the previous code multiplied by `16 px/line × 0.005 zoom/px` =
    // 0.08 zoom/click, way under the ~0.5 zoom/click a discrete
    // wheel click should feel like. Branch per delta-mode so each
    // input device gets a step calibrated to how it actually
    // generates events: trackpad PIXEL deltas stay continuous, mouse
    // LINE deltas get a real per-click step.
    //
    // Pan + zoom are independent listeners — the wheel fires
    // alongside any in-flight pointermove drag, so the user can
    // zoom while panning. The previous "doesn't zoom unless I stop
    // panning" was the trackpad step being so small that the camera
    // pan visually masked the zoom; the larger steps here make zoom
    // visible even mid-drag.
    attach(
        &canvas_target,
        "wheel",
        {
            let shared = shared.clone();
            let canvas = canvas.clone();
            let inner = inner.clone();
            Closure::wrap(Box::new(move |e: web_sys::Event| {
                if let Ok(w) = e.dyn_into::<web_sys::WheelEvent>() {
                    w.prevent_default();
                    let cursor = cursor_from_event(w.as_ref(), &canvas);
                    shared.cursor_px.set(cursor);
                    let zoom_delta = match w.delta_mode() {
                        // Mouse wheel: 0.5 zoom per click, sign-
                        // inverted so scroll-up zooms in.
                        web_sys::WheelEvent::DOM_DELTA_LINE => -w.delta_y() * 0.5,
                        // Page-scroll keys: a full zoom per page.
                        web_sys::WheelEvent::DOM_DELTA_PAGE => -w.delta_y(),
                        // Trackpad / smooth mouse: continuous pixel
                        // deltas; 0.01 zoom/px → ~1 zoom per 100 px
                        // of trackpad pan. Roughly 2x the previous
                        // (0.005) so a casual swipe lands ~2 zooms.
                        _ => -w.delta_y() * 0.01,
                    };
                    let canvas_size = inner.borrow().renderer.size();
                    inner
                        .borrow_mut()
                        .renderer
                        .user_zoom_at(zoom_delta, cursor, canvas_size);
                }
            }) as Box<dyn FnMut(web_sys::Event)>)
        },
        &mut listeners,
    )?;

    // Search-bar wiring — best-effort. If the host page doesn't
    // include `#aegis-search`, we silently skip it; the renderer
    // still works without a search box.
    let search_handles = attach_search_bar(&document, inner.clone())?;

    Ok(AegisInstance {
        inner,
        _raf: raf,
        _resize_observer: resize_observer,
        _resize_cb: resize_cb,
        _listeners: listeners,
        _search: search_handles,
    })
}

// ---------------------------------------------------------------------------
// Search bar — plan 0002 M2.
// ---------------------------------------------------------------------------

/// Shared state for the search-bar closures. All four event paths
/// (input, keydown, dropdown click, async fetch completion) touch
/// this through `Rc<RefCell<_>>`.
struct SearchState {
    results: Vec<SearchResult>,
    /// Index into `results` of the keyboard-highlighted row.
    /// `None` when no row is highlighted (initial state + after
    /// Escape).
    highlight: Option<usize>,
    /// JS handle of the pending debounce timeout, if any. Cleared
    /// before scheduling a new one so each keystroke produces at
    /// most one in-flight timer.
    timeout_handle: Option<i32>,
    /// Monotonically-increasing request generation. Async fetches
    /// stamp the value at dispatch; only the most recent
    /// generation's response is rendered, so a slow earlier
    /// request can't overwrite a faster later one.
    request_generation: u64,
    /// Geocoder client — Photon by default, switches to Nominatim
    /// after the first Photon error per session.
    geocoder: GeocoderClient,
    /// Most recently rendered query string. Used by the click
    /// handler to know what the user actually saw when they
    /// clicked a row.
    rendered_query: String,
}

/// Closures + listener handles kept alive for the lifetime of the
/// search bar. Dropping this struct (alongside the parent
/// `AegisInstance`) detaches every listener.
pub struct SearchHandles {
    _input_listener: ListenerHandle,
    _keydown_listener: ListenerHandle,
    _results_listener: ListenerHandle,
}

fn attach_search_bar(
    document: &web_sys::Document,
    inner: Rc<RefCell<Inner>>,
) -> Result<Option<SearchHandles>, JsValue> {
    let input_el = match document.get_element_by_id("aegis-search-input") {
        Some(el) => el,
        None => return Ok(None),
    };
    let input: web_sys::HtmlInputElement = input_el
        .dyn_into()
        .map_err(|_| JsValue::from_str("aegis-search-input is not an <input>"))?;
    let results_el = document
        .get_element_by_id("aegis-search-results")
        .ok_or_else(|| JsValue::from_str("missing #aegis-search-results"))?;

    let state = Rc::new(RefCell::new(SearchState {
        results: Vec::new(),
        highlight: None,
        timeout_handle: None,
        request_generation: 0,
        geocoder: GeocoderClient::new(),
        rendered_query: String::new(),
    }));

    // --- input: debounce 250 ms, then parse-or-geocode ---
    let input_listener = {
        let state = state.clone();
        let inner = inner.clone();
        let input_for_target: web_sys::EventTarget = input.clone().into();
        let input = input.clone();
        let results_el = results_el.clone();
        let document = document.clone();
        let closure = Closure::wrap(Box::new(move |_e: web_sys::Event| {
            let query = input.value();
            schedule_search(
                state.clone(),
                inner.clone(),
                results_el.clone(),
                document.clone(),
                query,
            );
        }) as Box<dyn FnMut(web_sys::Event)>);
        input_for_target
            .add_event_listener_with_callback("input", closure.as_ref().unchecked_ref())?;
        ListenerHandle {
            target: input_for_target,
            event: "input",
            closure,
        }
    };

    // --- keydown: ↑ / ↓ / Enter / Escape ---
    let keydown_listener = {
        let state = state.clone();
        let inner = inner.clone();
        let input_for_target: web_sys::EventTarget = input.clone().into();
        let input = input.clone();
        let results_el = results_el.clone();
        let document = document.clone();
        let closure = Closure::wrap(Box::new(move |e: web_sys::Event| {
            let kev: web_sys::KeyboardEvent = match e.dyn_into() {
                Ok(k) => k,
                Err(_) => return,
            };
            match kev.key().as_str() {
                "ArrowDown" => {
                    kev.prevent_default();
                    move_highlight(state.clone(), &results_el, 1);
                }
                "ArrowUp" => {
                    kev.prevent_default();
                    move_highlight(state.clone(), &results_el, -1);
                }
                "Enter" => {
                    kev.prevent_default();
                    commit_highlighted(
                        state.clone(),
                        inner.clone(),
                        &input,
                        &results_el,
                        &document,
                    );
                }
                "Escape" => {
                    kev.prevent_default();
                    clear_dropdown(state.clone(), &results_el);
                    input.blur().ok();
                }
                _ => {}
            }
        }) as Box<dyn FnMut(web_sys::Event)>);
        input_for_target
            .add_event_listener_with_callback("keydown", closure.as_ref().unchecked_ref())?;
        ListenerHandle {
            target: input_for_target,
            event: "keydown",
            closure,
        }
    };

    // --- click on a result row: commit that row ---
    let results_listener = {
        let state = state.clone();
        let inner = inner.clone();
        let results_target: web_sys::EventTarget = results_el.clone().into();
        let input = input.clone();
        let results_el_for_listener = results_el.clone();
        let document = document.clone();
        let closure = Closure::wrap(Box::new(move |e: web_sys::Event| {
            // Walk up from the click target until we find a row
            // with a data-idx attribute (the rows we render).
            let Some(target) = e.target() else { return };
            let mut node: Option<web_sys::Element> = target.dyn_into().ok();
            let idx = loop {
                let Some(el) = node.as_ref() else { break None };
                if let Some(s) = el.get_attribute("data-idx") {
                    break s.parse::<usize>().ok();
                }
                node = el.parent_element();
            };
            let Some(idx) = idx else { return };
            state.borrow_mut().highlight = Some(idx);
            commit_highlighted(
                state.clone(),
                inner.clone(),
                &input,
                &results_el_for_listener,
                &document,
            );
        }) as Box<dyn FnMut(web_sys::Event)>);
        results_target
            .add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())?;
        ListenerHandle {
            target: results_target,
            event: "click",
            closure,
        }
    };

    Ok(Some(SearchHandles {
        _input_listener: input_listener,
        _keydown_listener: keydown_listener,
        _results_listener: results_listener,
    }))
}

/// Cancel any pending debounce, then schedule a new 250 ms timeout
/// that runs the parse-or-geocode for `query`. If `query` is empty
/// (or whitespace-only) we clear the dropdown without scheduling.
fn schedule_search(
    state: Rc<RefCell<SearchState>>,
    inner: Rc<RefCell<Inner>>,
    results_el: web_sys::Element,
    document: web_sys::Document,
    query: String,
) {
    let win = web_window();
    {
        let mut s = state.borrow_mut();
        if let Some(handle) = s.timeout_handle.take() {
            win.clear_timeout_with_handle(handle);
        }
    }
    if query.trim().is_empty() {
        clear_dropdown(state.clone(), &results_el);
        return;
    }
    // Bump generation so any in-flight async fetch from earlier
    // becomes a no-op when it lands.
    let gen = {
        let mut s = state.borrow_mut();
        s.request_generation += 1;
        s.request_generation
    };

    let state_for_cb = state.clone();
    let inner_for_cb = inner.clone();
    let results_for_cb = results_el.clone();
    let doc_for_cb = document.clone();
    let query_for_cb = query.clone();

    let closure = Closure::once(Box::new(move || {
        // Clear timeout-handle bookkeeping; the timer just fired.
        state_for_cb.borrow_mut().timeout_handle = None;

        // Coord-parse path first — no network call, synthetic row.
        if let Some(lonlat) = parse_coord(&query_for_cb) {
            let synthetic = synthetic_coord_result(lonlat);
            render_results(
                &state_for_cb,
                &results_for_cb,
                &doc_for_cb,
                vec![synthetic],
                &query_for_cb,
            );
            return;
        }

        // Geocoder path — async. The captured `gen` guards against
        // stale responses.
        let state_async = state_for_cb.clone();
        let inner_async = inner_for_cb.clone();
        let results_async = results_for_cb.clone();
        let doc_async = doc_for_cb.clone();
        let query_async = query_for_cb.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let near = Some(inner_async.borrow().renderer.camera.center_lonlat);
            let mut client = { state_async.borrow().geocoder.clone() };
            let result = geocode_async(&mut client, &query_async, near).await;
            // Persist the (possibly switched-to-Nominatim) client.
            state_async.borrow_mut().geocoder = client;
            // Stale check — only render if we're still the latest.
            if state_async.borrow().request_generation != gen {
                return;
            }
            match result {
                Ok(results) => {
                    let mapped = if results.is_empty() {
                        vec![empty_state_row("no matches", &query_async)]
                    } else {
                        results
                    };
                    render_results(
                        &state_async,
                        &results_async,
                        &doc_async,
                        mapped,
                        &query_async,
                    );
                }
                Err(e) => {
                    log::warn!("geocode {query_async:?}: {e}");
                    render_results(
                        &state_async,
                        &results_async,
                        &doc_async,
                        vec![empty_state_row("geocoder unreachable", &query_async)],
                        &query_async,
                    );
                }
            }
        });
    }) as Box<dyn FnOnce()>);

    let handle = win
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            250,
        )
        .expect("set_timeout");
    closure.forget();
    state.borrow_mut().timeout_handle = Some(handle);
}

/// Build a synthetic `SearchResult` for a parsed coordinate. The
/// "kind" is `Unknown` so the click handler falls back to a sensible
/// default zoom (z=12) when no bbox is available — same as a city.
fn synthetic_coord_result((lon, lat): (f64, f64)) -> SearchResult {
    let ns = if lat >= 0.0 { 'N' } else { 'S' };
    let ew = if lon >= 0.0 { 'E' } else { 'W' };
    SearchResult {
        name: format!("{:.4}°{ns}, {:.4}°{ew}", lat.abs(), lon.abs()),
        context: "coordinate — click or press Enter to fly".to_string(),
        lonlat: (lon, lat),
        bbox: None,
        kind: ResultKind::City,
    }
}

/// Build a no-result row (no `bbox`, kind=`Unknown`) that lives in
/// the dropdown only to surface "no matches" / "geocoder
/// unreachable" to the user. The click handler treats these as
/// no-ops (no lonlat to fly to — we set it to the camera centre,
/// but commit_highlighted will short-circuit on the synthetic name
/// containing `'·'`).
fn empty_state_row(label: &str, query: &str) -> SearchResult {
    SearchResult {
        name: label.to_string(),
        context: format!("· {query}"),
        lonlat: (0.0, 0.0),
        bbox: None,
        kind: ResultKind::Unknown,
    }
}

/// Render `results` into the dropdown DOM and update `state`. Each
/// row carries a `data-idx="<i>"` attribute so the delegated click
/// listener can map back to the result index.
fn render_results(
    state: &Rc<RefCell<SearchState>>,
    container: &web_sys::Element,
    document: &web_sys::Document,
    results: Vec<SearchResult>,
    query: &str,
) {
    // Clear children.
    while let Some(child) = container.first_child() {
        let _ = container.remove_child(&child);
    }
    for (idx, r) in results.iter().enumerate() {
        let row = match document.create_element("div") {
            Ok(el) => el,
            Err(_) => continue,
        };
        row.set_attribute("role", "option").ok();
        row.set_attribute("data-idx", &idx.to_string()).ok();
        let name_el = match document.create_element("div") {
            Ok(el) => el,
            Err(_) => continue,
        };
        name_el.set_class_name("row-name");
        name_el.set_text_content(Some(&r.name));
        let ctx_el = match document.create_element("div") {
            Ok(el) => el,
            Err(_) => continue,
        };
        ctx_el.set_class_name("row-context");
        ctx_el.set_text_content(Some(&r.context));
        let _ = row.append_child(&name_el);
        let _ = row.append_child(&ctx_el);
        let _ = container.append_child(&row);
    }
    let mut s = state.borrow_mut();
    s.results = results;
    s.highlight = if s.results.is_empty() { None } else { Some(0) };
    s.rendered_query = query.to_string();
    drop(s);
    refresh_selection(state, container);
}

/// Sync the `aria-selected` attribute on every row with the state's
/// `highlight` index. The CSS uses `[aria-selected="true"]` for the
/// highlight style.
fn refresh_selection(state: &Rc<RefCell<SearchState>>, container: &web_sys::Element) {
    let highlight = state.borrow().highlight;
    let children = container.children();
    for i in 0..children.length() {
        let Some(child) = children.item(i) else {
            continue;
        };
        let selected = highlight == Some(i as usize);
        child
            .set_attribute("aria-selected", if selected { "true" } else { "false" })
            .ok();
    }
}

fn clear_dropdown(state: Rc<RefCell<SearchState>>, container: &web_sys::Element) {
    while let Some(child) = container.first_child() {
        let _ = container.remove_child(&child);
    }
    let mut s = state.borrow_mut();
    s.results.clear();
    s.highlight = None;
}

fn move_highlight(state: Rc<RefCell<SearchState>>, container: &web_sys::Element, delta: i32) {
    let mut s = state.borrow_mut();
    if s.results.is_empty() {
        return;
    }
    let len = s.results.len() as i32;
    let cur = s.highlight.map_or(-1, |i| i as i32);
    let next = (cur + delta).rem_euclid(len);
    s.highlight = Some(next as usize);
    drop(s);
    refresh_selection(&state, container);
}

fn commit_highlighted(
    state: Rc<RefCell<SearchState>>,
    inner: Rc<RefCell<Inner>>,
    input: &web_sys::HtmlInputElement,
    container: &web_sys::Element,
    _document: &web_sys::Document,
) {
    let (selected, _query) = {
        let s = state.borrow();
        let Some(idx) = s.highlight else {
            return;
        };
        let Some(r) = s.results.get(idx) else {
            return;
        };
        (r.clone(), s.rendered_query.clone())
    };
    // Empty-state rows have context starting with `·` — they're
    // not real results and shouldn't move the camera.
    if selected.context.starts_with('·') {
        return;
    }
    let now = web_window().performance().map_or(0.0, |p| p.now() / 1000.0);
    {
        let mut inner_mut = inner.borrow_mut();
        match selected.bbox {
            Some(b) => inner_mut.renderer.fly_to_bbox(b, now),
            None => inner_mut
                .renderer
                .fly_to(selected.lonlat, selected.kind.default_zoom(), now),
        }
    }
    // Hide the dropdown + blur — committed.
    clear_dropdown(state, container);
    input.blur().ok();
    input.set_value("");
}
