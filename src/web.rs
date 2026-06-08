//! wasm-bindgen entry points for the browser build.
//!
//! M0 stub — exposes a `start(host_id)` that attaches to a `<canvas>` and
//! logs a hello-world line via `console.log`. The wgpu surface attachment
//! and the actual render loop land in M0's final pass.

use wasm_bindgen::prelude::*;

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

/// Attach to a host element by id and log a hello line.
///
/// The actual wgpu surface attachment + render loop is added in the
/// rest of M0; this is the first wasm entry point the test harness
/// can call.
#[wasm_bindgen]
pub fn start(host_id: &str) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let host = document
        .get_element_by_id(host_id)
        .ok_or_else(|| JsValue::from_str(&format!("no element with id={host_id}")))?;

    log::info!(
        "aegis {} attaching to <{}>",
        crate::version::VERSION,
        host.tag_name().to_lowercase(),
    );
    Ok(())
}
