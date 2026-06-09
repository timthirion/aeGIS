//! Shared HTTP-bytes plumbing for tile fetches, geocoder queries,
//! and anything else that needs an idiomatic GET-and-give-me-the-
//! body call on both native and web.
//!
//! Native: `ehttp::fetch_blocking`. Web: `web_sys::fetch`. Both
//! return `Result<Vec<u8>, NetError>` — the caller decides how to
//! interpret those bytes (decode as an image, parse as JSON,
//! treat as raw bytes for an upload).
//!
//! Before this module landed, `tile::fetch_tile_*` was the only
//! HTTP shape in the codebase and it ran the body through
//! `decode_image` before returning, so no other consumer could
//! reuse it. The tile fetcher now sits on `net::fetch_bytes_*`
//! and the search module's geocoder client sits on the same.

use thiserror::Error;

/// Errors a `fetch_bytes_*` call can surface. Keeping these
/// distinct lets the caller decide whether to retry (transient
/// transport) vs give up (4xx URL the server understood and
/// rejected) vs flag a programming bug (response body shape
/// surprises further up the stack).
#[derive(Debug, Error)]
pub enum NetError {
    /// Network-layer failure — DNS, TCP, TLS, browser CORS, etc.
    /// The message is the underlying error's `Display`.
    #[error("transport: {0}")]
    Transport(String),
    /// HTTP response received but status was not 2xx.
    #[error("HTTP {status} for {url}")]
    HttpStatus { status: u16, url: String },
}

/// `User-Agent` header value used by every native fetch. Identifies
/// the project to any provider that logs / rate-limits by UA. The
/// browser sets its own UA on web; we don't override it there.
pub const USER_AGENT: &str = concat!(
    "aegis/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/timthirion/aeGIS)"
);

/// Native-only synchronous bytes fetch. Blocks the caller until the
/// response is fully buffered. Use only from worker threads;
/// the native frame loop spawns a thread per fetch (see
/// `render::Renderer::dispatch_*_tile_fetch`).
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_bytes_blocking(url: &str) -> Result<Vec<u8>, NetError> {
    let request = ehttp::Request {
        headers: ehttp::Headers::new(&[("User-Agent", USER_AGENT), ("Accept", "*/*")]),
        ..ehttp::Request::get(url)
    };
    let response =
        ehttp::fetch_blocking(&request).map_err(|e| NetError::Transport(e.to_string()))?;
    if !response.ok {
        return Err(NetError::HttpStatus {
            status: response.status,
            url: url.to_owned(),
        });
    }
    Ok(response.bytes)
}

/// Web-only async bytes fetch. Uses `web_sys::fetch` directly so
/// the returned future is `!Send` and can capture the `Rc<RefCell
/// <Inner>>` the web entry-point uses for shared state.
#[cfg(target_arch = "wasm32")]
pub async fn fetch_bytes_async(url: &str) -> Result<Vec<u8>, NetError> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    let window = web_sys::window().ok_or_else(|| NetError::Transport("no window".into()))?;
    let resp_value = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|e| NetError::Transport(format!("{e:?}")))?;
    let resp: web_sys::Response = resp_value
        .dyn_into()
        .map_err(|e| NetError::Transport(format!("Response cast: {e:?}")))?;
    if !resp.ok() {
        return Err(NetError::HttpStatus {
            status: resp.status(),
            url: url.to_owned(),
        });
    }
    let buffer = JsFuture::from(
        resp.array_buffer()
            .map_err(|e| NetError::Transport(format!("array_buffer: {e:?}")))?,
    )
    .await
    .map_err(|e| NetError::Transport(format!("array_buffer await: {e:?}")))?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}
