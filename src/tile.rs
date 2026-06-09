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
    /// Esri ArcGIS World Imagery. JPEG, 256×256, served from Esri's
    /// global CDN. Free to use with attribution per Esri's basemap
    /// terms; downstream deployments must surface the credit
    /// "Source: Esri, Maxar, Earthstar Geographics, and the GIS User
    /// Community" in the visible UI. We picked this over EOX's
    /// Sentinel-2 cloudless after measuring ~6–10× lower latency from
    /// the CDN — the prior source was leaving the visible hemisphere
    /// patchy for seconds at a time.
    EsriWorldImagery,
}

/// Highest zoom the Esri World Imagery layer reliably publishes
/// worldwide. A handful of urban areas extend to z=20–23, but z=19 is
/// the conservative cap that's covered everywhere the user might pan.
pub const ESRI_WORLD_IMAGERY_MAX_Z: u8 = 19;

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
    /// **Esri World Imagery** (Satellite mode): 256×256 JPEG from
    /// Esri's `services.arcgisonline.com` tile endpoint. Aggregated
    /// imagery (Maxar, Earthstar, etc.) served from a global CDN.
    /// Free to use with attribution per Esri's basemap terms. Note
    /// the path order is `{z}/{y}/{x}` — row before column — not the
    /// XYZ slippy convention.
    pub fn tile_url(&self, provider: TileProvider) -> String {
        match provider {
            TileProvider::Carto => format!(
                "https://a.basemaps.cartocdn.com/rastertiles/voyager/{z}/{x}/{y}@2x.png",
                z = self.z,
                x = self.x,
                y = self.y,
            ),
            TileProvider::EsriWorldImagery => format!(
                "https://services.arcgisonline.com/ArcGIS/rest/services/World_Imagery/\
                 MapServer/tile/{z}/{y}/{x}",
                z = self.z,
                x = self.x,
                y = self.y,
            ),
        }
    }
}

/// `User-Agent` value for native HTTP. Re-export of `net::USER_AGENT`
/// kept for backwards-compat with the existing test that pins the
/// project-identifying shape.
pub const TILE_USER_AGENT: &str = crate::net::USER_AGENT;

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

/// Native-only synchronous tile fetch. Bytes from `net::fetch_bytes_blocking`,
/// then `decode_image` for the format autodetect (PNG for Carto, JPEG for
/// Esri World Imagery — both providers share this path).
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch_tile_blocking(url: &str) -> Result<DecodedTile, String> {
    let bytes = crate::net::fetch_bytes_blocking(url).map_err(|e| format!("fetch: {e}"))?;
    decode_image(&bytes).map_err(|e| format!("decode: {e}"))
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

/// Async tile fetch via the browser's `fetch` API. Bytes from
/// `net::fetch_bytes_async`, then `decode_image` for format
/// autodetect. Used by the renderer's per-tile dispatcher
/// (spawn_local).
#[cfg(target_arch = "wasm32")]
pub async fn fetch_tile_web(url: &str) -> Result<DecodedTile, String> {
    let bytes = crate::net::fetch_bytes_async(url)
        .await
        .map_err(|e| format!("fetch: {e}"))?;
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
    fn esri_world_imagery_url_uses_zyx_order() {
        let tile = TileId { z: 5, x: 9, y: 12 };
        assert_eq!(
            tile.tile_url(TileProvider::EsriWorldImagery),
            "https://services.arcgisonline.com/ArcGIS/rest/services/World_Imagery/\
             MapServer/tile/5/12/9"
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
