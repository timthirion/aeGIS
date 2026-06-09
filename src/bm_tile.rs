//! Streaming Blue Marble tiles from NASA GIBS.
//!
//! NASA's Global Imagery Browse Services (GIBS) serves the
//! `BlueMarble_ShadedRelief_Bathymetry` layer as a standard WMTS
//! pyramid in **EPSG:4326** (equirectangular), JPEG-encoded, CORS-
//! enabled, no API key required. Pyramid depth: z=0 through z=7.
//! Effective resolution at the deepest level: ~8.6 gigapixels — about
//! 700× more pixels than the bundled 4096×2048 base texture.
//!
//! ## EPSG:4326 vs EPSG:3857 (Carto basemap)
//!
//! The Carto basemap tiles we already fetch use Web Mercator
//! (EPSG:3857) — square tiles in projected world space, lat-clamped
//! to ±85.05°. BM tiles are EPSG:4326 — square tiles in *degree*
//! space, with z=0 being **two tiles wide × one tile tall** to keep
//! the world's 2:1 aspect ratio. At zoom z each axis halves:
//! - tiles wide: `2 · 2^z`
//! - tiles tall: `2^z`
//! - tile lon span: `360 / (2 · 2^z) = 180 / 2^z` degrees
//! - tile lat span: `180 / 2^z` degrees
//!
//! Each tile's bounding box is straightforward in degrees; the
//! globe-rendering vertex shader takes those degrees directly and
//! projects to the sphere via `lonlat_to_sphere`, no Mercator math
//! involved. That projection-split is the entire motivation for the
//! [follow-on plan]'s "per-pipeline shader" choice.

use crate::camera::Camera;
use crate::crs;

/// WMTS endpoint for the BlueMarble_ShadedRelief_Bathymetry layer.
/// JPEG, EPSG:4326, 500m at the deepest zoom.
const GIBS_BASE_URL: &str =
    "https://gibs.earthdata.nasa.gov/wmts/epsg4326/best/BlueMarble_ShadedRelief_Bathymetry/default/500m";

/// Highest zoom GIBS publishes for this layer. z=8 returns 400.
pub const BM_MAX_Z: u8 = 7;

/// Zoom level the bundled 4096×2048 Earth texture is equivalent to
/// in the GIBS pyramid (1024×2^z × 512×2^z pixel resolution → z=2
/// matches 4096×2048). Streaming below this is wasted bandwidth —
/// the bundled base is already at-or-better than what GIBS serves.
pub const BM_BUNDLED_EQUIV_Z: u8 = 2;

/// A Blue Marble tile address — same XYZ shape as our Carto-side
/// [`crate::tile::TileId`], but tiles are in EPSG:4326 (degrees of
/// lon × lat) not Web Mercator. Kept as a distinct type so the
/// projection mismatch is impossible to confuse at a call site.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct BmTileId {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl BmTileId {
    /// GIBS WMTS endpoint URL for this tile. The WMTS path order is
    /// `{TileMatrix}/{TileRow}/{TileCol}` = `{z}/{y}/{x}`.
    pub fn url(&self) -> String {
        format!(
            "{base}/{z}/{y}/{x}.jpeg",
            base = GIBS_BASE_URL,
            z = self.z,
            y = self.y,
            x = self.x,
        )
    }

    /// Tile's geographic bounding box in **degrees**:
    /// `[lon_min, lat_min, lon_max, lat_max]`. The shader's per-vertex
    /// `(lon, lat)` interpolation reads these straight out of the
    /// per-tile uniform.
    pub fn lon_lat_bounds(&self) -> [f32; 4] {
        let n_x = (2u32 << self.z) as f32; // tiles wide
        let n_y = (1u32 << self.z) as f32; // tiles tall
        let lon_min = -180.0 + 360.0 * (self.x as f32) / n_x;
        let lon_max = lon_min + 360.0 / n_x;
        let lat_max = 90.0 - 180.0 * (self.y as f32) / n_y;
        let lat_min = lat_max - 180.0 / n_y;
        [lon_min, lat_min, lon_max, lat_max]
    }
}

/// Pick the GIBS zoom level whose tile resolution best matches the
/// current camera scale. Returns `None` when the bundled base
/// texture is already at-or-better than what GIBS would serve at
/// `camera.zoom` — there's no point spending bandwidth in that case.
///
/// Heuristic: GIBS at zoom `Z` provides ~`2.84 · 2^Z` pixels per
/// degree of lon. The camera at zoom `X` paints
/// `pixels_per_world / 360 = 256 · 2^X / 360 ≈ 0.71 · 2^X` device
/// pixels per degree. Sharp sampling requires GIBS_ppd ≥ camera_ppd
/// → `Z ≥ X − 2`. So we pick `round(X − 2)`, clamped to
/// `[BM_BUNDLED_EQUIV_Z + 1, BM_MAX_Z]`, returning `None` if the
/// result clamps back down to the bundled equivalent.
pub fn gibs_zoom_for(camera_zoom: f64) -> Option<u8> {
    let suggested = (camera_zoom - 2.0).round() as i32;
    let max = BM_MAX_Z as i32;
    let min = BM_BUNDLED_EQUIV_Z as i32 + 1;
    if suggested < min {
        None
    } else {
        Some(suggested.min(max) as u8)
    }
}

/// The set of BM tiles covering the camera's current viewport at
/// `gibs_z`. Uses flat-Mercator math to compute the visible bounds
/// (consistent with [`Camera::visible_tiles`]), then maps that lat/lon
/// rectangle onto the EPSG:4326 tile grid.
///
/// Antimeridian wrap is **not** handled — same limitation as the
/// Carto-side `visible_tiles`. A viewport straddling ±180° lon will
/// return only the half on one side.
pub fn visible_tiles(camera: &Camera, canvas: (u32, u32), gibs_z: u8) -> Vec<BmTileId> {
    let n_x = (2u32 << gibs_z) as i64; // tiles wide
    let n_y = (1u32 << gibs_z) as i64; // tiles tall

    let ppw = camera.pixels_per_world();
    let half_w_world = canvas.0 as f64 / 2.0 / ppw;
    let half_h_world = canvas.1 as f64 / 2.0 / ppw;
    let (wcx, wcy) = crs::lonlat_to_world(camera.center_lonlat.0, camera.center_lonlat.1);
    let left_w = (wcx - half_w_world).clamp(0.0, 1.0);
    let right_w = (wcx + half_w_world).clamp(0.0, 1.0);
    let top_w = (wcy - half_h_world).clamp(0.0, 1.0);
    let bottom_w = (wcy + half_h_world).clamp(0.0, 1.0);

    let lon_min = left_w * 360.0 - 180.0;
    let lon_max = right_w * 360.0 - 180.0;
    let (_, lat_max) = crs::world_to_lonlat(0.5, top_w);
    let (_, lat_min) = crs::world_to_lonlat(0.5, bottom_w);

    let tile_x = |lon: f64| ((lon + 180.0) / 360.0 * n_x as f64).floor() as i64;
    let tile_y = |lat: f64| ((90.0 - lat) / 180.0 * n_y as f64).floor() as i64;
    let x_min = tile_x(lon_min).clamp(0, n_x - 1);
    let x_max = tile_x(lon_max).clamp(0, n_x - 1);
    let y_min = tile_y(lat_max).clamp(0, n_y - 1);
    let y_max = tile_y(lat_min).clamp(0, n_y - 1);

    let mut tiles = Vec::with_capacity(((x_max - x_min + 1) * (y_max - y_min + 1)) as usize);
    for ty in y_min..=y_max {
        for tx in x_min..=x_max {
            tiles.push(BmTileId {
                z: gibs_z,
                x: tx as u32,
                y: ty as u32,
            });
        }
    }
    tiles
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tile::CHICAGO_LONLAT;

    #[test]
    fn z0_world_split_in_two_tiles() {
        // EPSG:4326 z=0 has 2 tiles wide × 1 tall (world is 2:1).
        let west = BmTileId { z: 0, x: 0, y: 0 };
        let east = BmTileId { z: 0, x: 1, y: 0 };
        assert_eq!(west.lon_lat_bounds(), [-180.0, -90.0, 0.0, 90.0]);
        assert_eq!(east.lon_lat_bounds(), [0.0, -90.0, 180.0, 90.0]);
    }

    #[test]
    fn z1_quadrants() {
        // At z=1, 4 tiles wide × 2 tall, each 90° × 90°.
        let nw = BmTileId { z: 1, x: 0, y: 0 }.lon_lat_bounds();
        let ne = BmTileId { z: 1, x: 3, y: 0 }.lon_lat_bounds();
        let sw = BmTileId { z: 1, x: 0, y: 1 }.lon_lat_bounds();
        let se = BmTileId { z: 1, x: 3, y: 1 }.lon_lat_bounds();
        // NW: lon −180 → −90, lat 0 → 90
        assert_eq!(nw, [-180.0, 0.0, -90.0, 90.0]);
        // NE: lon 90 → 180, lat 0 → 90
        assert_eq!(ne, [90.0, 0.0, 180.0, 90.0]);
        // SW: lon −180 → −90, lat −90 → 0
        assert_eq!(sw, [-180.0, -90.0, -90.0, 0.0]);
        // SE: lon 90 → 180, lat −90 → 0
        assert_eq!(se, [90.0, -90.0, 180.0, 0.0]);
    }

    #[test]
    fn url_uses_wmts_zyx_order() {
        let t = BmTileId { z: 3, x: 5, y: 2 };
        assert_eq!(
            t.url(),
            "https://gibs.earthdata.nasa.gov/wmts/epsg4326/best/\
             BlueMarble_ShadedRelief_Bathymetry/default/500m/3/2/5.jpeg"
        );
    }

    #[test]
    fn gibs_zoom_for_low_camera_returns_none() {
        // Bundled 4096×2048 matches GIBS z=2. Below camera zoom 4 (where
        // z = round(camera_zoom - 2) ≤ 2) streaming buys nothing.
        for cam in [0.0, 1.0, 2.0, 3.0, 3.4] {
            assert_eq!(gibs_zoom_for(cam), None, "camera zoom {cam}");
        }
    }

    #[test]
    fn gibs_zoom_for_mid_camera_picks_higher_z() {
        // Camera zoom 4.5 → round(2.5) = 3.
        assert_eq!(gibs_zoom_for(4.5), Some(3));
        assert_eq!(gibs_zoom_for(5.0), Some(3));
        assert_eq!(gibs_zoom_for(6.0), Some(4));
    }

    #[test]
    fn gibs_zoom_for_clamps_to_max() {
        // Beyond GIBS's deepest published level (z=7), clamp.
        assert_eq!(gibs_zoom_for(15.0), Some(BM_MAX_Z));
        assert_eq!(gibs_zoom_for(19.0), Some(BM_MAX_Z));
    }

    #[test]
    fn visible_tiles_at_z0_around_chicago_returns_west_tile() {
        // Chicago is in the western hemisphere → tile (z=0, x=0, y=0).
        let cam = Camera::new(CHICAGO_LONLAT.0, CHICAGO_LONLAT.1, 5.0);
        let tiles = visible_tiles(&cam, (800, 600), 0);
        assert!(
            tiles.contains(&BmTileId { z: 0, x: 0, y: 0 }),
            "expected west z=0 tile, got {tiles:?}"
        );
    }

    #[test]
    fn visible_tiles_at_z3_around_chicago_includes_chicago_tile() {
        // At GIBS z=3, 16 tiles wide × 8 tall. Each is 22.5° × 22.5°.
        // Chicago (−87.6, 41.9) → x = floor((−87.6 + 180) / 22.5) = 4,
        // y = floor((90 − 41.9) / 22.5) = 2.
        let cam = Camera::new(CHICAGO_LONLAT.0, CHICAGO_LONLAT.1, 6.0);
        let tiles = visible_tiles(&cam, (800, 600), 3);
        assert!(
            tiles.contains(&BmTileId { z: 3, x: 4, y: 2 }),
            "Chicago z=3 tile not in viewport: {tiles:?}"
        );
    }

    #[test]
    fn visible_tiles_z0_chicago_returns_one_tile_only() {
        // At z=0 the camera viewport — even at low camera zoom —
        // shouldn't span both hemispheres given a typical viewport.
        // Just confirm the count is bounded.
        let cam = Camera::new(CHICAGO_LONLAT.0, CHICAGO_LONLAT.1, 5.0);
        let tiles = visible_tiles(&cam, (800, 600), 0);
        assert!(
            (1..=2).contains(&tiles.len()),
            "unexpected tile count: {}",
            tiles.len()
        );
    }
}
