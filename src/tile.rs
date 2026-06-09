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

/// Which provider's tile pyramid a `TileId` resolves to. Both
/// providers use the same Web Mercator XYZ pyramid (so the geometry
/// math is identical), only the URL and the practical max-zoom
/// differ.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TileProvider {
    /// Carto Voyager OSM-derived street basemap. PNG, retina @2x.
    Carto,
    /// EOX-hosted Sentinel-2 cloudless mosaic. JPEG, 256×256, CC BY 4.0.
    /// Requires the attribution
    /// "Sentinel-2 cloudless by EOX IT Services GmbH" to be visible.
    Sentinel2Cloudless,
}

/// Highest zoom the Sentinel-2 cloudless layer publishes. Source
/// resolution is ~10 m/pixel; finer than that is just JPEG stretch.
pub const SENTINEL2_MAX_Z: u8 = 14;

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

    /// The tile's extent in normalised Mercator world coords as
    /// `(x_min, y_min, x_max, y_max)`. The tessellated globe-tile
    /// shader uses this to interpolate per-vertex world positions
    /// across the tile.
    pub fn world_rect(&self) -> [f32; 4] {
        let n = (1u32 << self.z) as f32;
        [
            self.x as f32 / n,
            self.y as f32 / n,
            (self.x + 1) as f32 / n,
            (self.y + 1) as f32 / n,
        ]
    }

    /// URL for this tile from the given provider. Both providers
    /// share the Web Mercator XYZ pyramid, so the `(z, x, y)` triple
    /// is provider-independent — only the endpoint changes.
    ///
    /// **Carto Voyager** (Map mode): 512×512 PNG retina tile. CORS-
    /// enabled, no API key, OSM-derived. The `@2x` form has 4× the
    /// pixel density of the standard 256×256 tiles so labels stay
    /// crisp on high-DPR displays. Attribution: © OpenStreetMap
    /// contributors © CARTO.
    ///
    /// **Sentinel-2 cloudless** (Satellite mode): 256×256 JPEG from
    /// EOX's WMTS REST endpoint. Sentinel-2 native ~10 m/pixel,
    /// global cloudless mosaic. License: CC BY 4.0. Attribution:
    /// Sentinel-2 cloudless by EOX IT Services GmbH.
    pub fn tile_url(&self, provider: TileProvider) -> String {
        match provider {
            TileProvider::Carto => format!(
                "https://a.basemaps.cartocdn.com/rastertiles/voyager/{z}/{x}/{y}@2x.png",
                z = self.z,
                x = self.x,
                y = self.y,
            ),
            TileProvider::Sentinel2Cloudless => format!(
                "https://tiles.maps.eox.at/wmts/1.0.0/s2cloudless-2020_3857/default/g/\
                 {z}/{y}/{x}.jpg",
                z = self.z,
                x = self.x,
                y = self.y,
            ),
        }
    }
}

/// `User-Agent` value for native HTTP — identifies the project to any
/// tile provider that logs / rate-limits by UA. Carto doesn't require
/// it, but well-behaved clients still send one.
pub const TILE_USER_AGENT: &str = concat!(
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

/// Decode raster bytes of any format the `image` crate's enabled
/// features support (PNG + JPEG today). Used by the bundled Blue
/// Marble Earth texture, which ships as JPEG to keep the wasm bundle
/// small; tiles stay on the format-specific [`decode_png`] path for
/// the explicit-format guarantee.
pub fn decode_image(bytes: &[u8]) -> Result<DecodedTile, image::ImageError> {
    let img = image::load_from_memory(bytes)?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(DecodedTile {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

/// Native-only synchronous tile fetch. Sends an HTTP GET with the
/// project's `User-Agent` and autodetects the response format (PNG
/// for Carto, JPEG for Sentinel-2 cloudless — both providers share
/// this path).
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_tile_blocking(url: &str) -> Result<DecodedTile, String> {
    let request = ehttp::Request {
        headers: ehttp::Headers::new(&[("User-Agent", TILE_USER_AGENT), ("Accept", "image/*")]),
        ..ehttp::Request::get(url)
    };
    let response = ehttp::fetch_blocking(&request).map_err(|e| format!("fetch: {e}"))?;
    if !response.ok {
        return Err(format!("HTTP {} for {}", response.status, url));
    }
    decode_image(&response.bytes).map_err(|e| format!("decode: {e}"))
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
    decode_image(&bytes).map_err(|e| format!("decode: {e}"))
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
            tile.tile_url(TileProvider::Carto),
            "https://a.basemaps.cartocdn.com/rastertiles/voyager/10/262/380@2x.png"
        );
    }

    #[test]
    fn sentinel2_url_uses_eox_wmts_zyx_order() {
        let tile = TileId { z: 5, x: 9, y: 12 };
        assert_eq!(
            tile.tile_url(TileProvider::Sentinel2Cloudless),
            "https://tiles.maps.eox.at/wmts/1.0.0/s2cloudless-2020_3857/default/g/\
             5/12/9.jpg"
        );
    }

    #[test]
    fn user_agent_identifies_project_and_version() {
        // Well-behaved clients always send a real UA so the tile host
        // can correlate any traffic spike to its origin.
        assert!(TILE_USER_AGENT.starts_with("aegis/"));
        assert!(TILE_USER_AGENT.contains(env!("CARGO_PKG_VERSION")));
        assert!(TILE_USER_AGENT.contains("github.com"));
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
