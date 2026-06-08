//! XYZ tiles: identity, math, and URL formation.
//!
//! A `TileId` is a `(z, x, y)` triple under the OSM / web-mapping
//! convention: zoom `z`, integer tile column `x` (east-increasing),
//! integer tile row `y` (south-increasing). The world fits in `(0, 0, 0)`;
//! at zoom `z` there are `2^z × 2^z` tiles.

use crate::crs;

/// An XYZ tile address.
///
/// Equality + hashing are derived so a `TileId` can serve as the cache
/// key once we land an LRU (plan 0001 M2).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileId {
    /// The tile containing `(lon, lat)` at zoom `z`. Tile coordinates
    /// are clamped to `[0, 2^z)` so a longitude slightly past the
    /// antimeridian (or a latitude past the projection's clamp) still
    /// produces a valid in-range tile.
    pub fn from_lonlat(z: u8, lon: f64, lat: f64) -> TileId {
        let (fx, fy) = crs::lonlat_to_tile_fractional(z, lon, lat);
        let n = 1u32 << z;
        let max = n.saturating_sub(1);
        let x = (fx.floor() as i64).clamp(0, max as i64) as u32;
        let y = (fy.floor() as i64).clamp(0, max as i64) as u32;
        TileId { z, x, y }
    }

    /// URL for a 256×256 PNG raster tile from OpenStreetMap's standard
    /// tile CDN. **Development / low-volume use only** per the
    /// [OSM tile-usage policy](https://operations.osmfoundation.org/policies/tiles/);
    /// production deployments must self-host (plan 0004 will introduce
    /// the PMTiles path).
    ///
    /// OSM requires a non-default `User-Agent` on every request — see
    /// [`OSM_USER_AGENT`].
    pub fn osm_url(&self) -> String {
        format!(
            "https://tile.openstreetmap.org/{z}/{x}/{y}.png",
            z = self.z,
            x = self.x,
            y = self.y,
        )
    }
}

/// `User-Agent` header value to send with OSM tile requests. OSM's tile
/// CDN refuses traffic with `User-Agent: libcurl/...` or other generic
/// values — see their usage policy. We identify as the project so
/// they can correlate any future traffic spike to its origin.
pub const OSM_USER_AGENT: &str = concat!(
    "aegis/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/timthirion/aeGIS)"
);

/// Longitude/latitude of a notable place — used as the default centre
/// for plan 0001's "first tile" demo.
pub const CHICAGO_LONLAT: (f64, f64) = (-87.6298, 41.8781);

/// Decoded raster tile: RGBA8 byte buffer plus its dimensions in pixels.
#[derive(Debug, Clone)]
pub struct DecodedTile {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Decode PNG bytes from the wire into [`DecodedTile`]. Pure Rust on
/// both targets (the `image` crate's `png` feature has no C deps).
pub fn decode_png(bytes: &[u8]) -> Result<DecodedTile, image::ImageError> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(DecodedTile {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

/// Native-only synchronous tile fetch. Sends an HTTP GET with the
/// project's `User-Agent`, decodes the PNG response into RGBA. Used by
/// the native entry to load the startup tile before entering the
/// event loop.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_tile_blocking(url: &str) -> Result<DecodedTile, String> {
    let request = ehttp::Request {
        headers: ehttp::Headers::new(&[("User-Agent", OSM_USER_AGENT), ("Accept", "image/png")]),
        ..ehttp::Request::get(url)
    };
    let response = ehttp::fetch_blocking(&request).map_err(|e| format!("fetch: {e}"))?;
    if !response.ok {
        return Err(format!("HTTP {} for {}", response.status, url));
    }
    decode_png(&response.bytes).map_err(|e| format!("decode_png: {e}"))
}

/// Web-only async tile fetch. Spawns a task on the browser's event
/// loop; `on_done` runs with the decoded tile (or an error string)
/// when the fetch completes. Uses `web_sys::fetch` directly — the
/// browser sets `User-Agent`, and the closure stays `!Send` so it can
/// capture the `Rc<RefCell<Inner>>` the web entry uses.
#[cfg(target_arch = "wasm32")]
pub fn fetch_tile_async(url: &str, on_done: impl 'static + FnOnce(Result<DecodedTile, String>)) {
    let url = url.to_owned();
    wasm_bindgen_futures::spawn_local(async move {
        on_done(fetch_tile_web(&url).await);
    });
}

/// Async tile fetch via the browser's `fetch` API. Used by the
/// renderer's per-tile dispatcher (spawn_local).
#[cfg(target_arch = "wasm32")]
pub async fn fetch_tile_web(url: &str) -> Result<DecodedTile, String> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    let window = web_sys::window().ok_or("no global window")?;
    let resp_value = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|e| format!("fetch: {e:?}"))?;
    let resp: web_sys::Response = resp_value
        .dyn_into()
        .map_err(|e| format!("Response cast: {e:?}"))?;
    if !resp.ok() {
        return Err(format!("HTTP {} for {}", resp.status(), url));
    }
    let buffer = JsFuture::from(
        resp.array_buffer()
            .map_err(|e| format!("array_buffer: {e:?}"))?,
    )
    .await
    .map_err(|e| format!("array_buffer await: {e:?}"))?;
    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
    decode_png(&bytes).map_err(|e| format!("decode_png: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z0_world_tile() {
        // Anywhere on Earth at z=0 → tile (0, 0, 0).
        for (lon, lat) in [(0.0, 0.0), (180.0, 0.0), (-180.0, 0.0), (0.0, 85.0)] {
            assert_eq!(
                TileId::from_lonlat(0, lon, lat),
                TileId { z: 0, x: 0, y: 0 },
                "z=0 at ({lon}, {lat})"
            );
        }
    }

    #[test]
    fn chicago_tile_z10() {
        let (lon, lat) = CHICAGO_LONLAT;
        let tile = TileId::from_lonlat(10, lon, lat);
        assert_eq!(
            tile,
            TileId {
                z: 10,
                x: 262,
                y: 380
            }
        );
        assert_eq!(
            tile.osm_url(),
            "https://tile.openstreetmap.org/10/262/380.png"
        );
    }

    #[test]
    fn user_agent_identifies_project_and_version() {
        // Important: OSM uses User-Agent to track + (if needed) rate-
        // limit specific applications. The string must identify both.
        assert!(OSM_USER_AGENT.starts_with("aegis/"));
        assert!(OSM_USER_AGENT.contains(env!("CARGO_PKG_VERSION")));
        assert!(OSM_USER_AGENT.contains("github.com"));
    }

    #[test]
    fn tile_clamps_at_world_edges() {
        // Latitudes past the Mercator clamp shouldn't escape the tile
        // grid even though the fractional math would push `fy` past
        // `n`. (Defensive — `lonlat_to_world` clamps internally too,
        // but the clamp here defends against future drift.)
        let north = TileId::from_lonlat(5, 0.0, 89.0);
        assert!(north.y < (1u32 << 5), "north tile y in range: {north:?}");
        let south = TileId::from_lonlat(5, 0.0, -89.0);
        assert!(south.y < (1u32 << 5), "south tile y in range: {south:?}");
    }
}
