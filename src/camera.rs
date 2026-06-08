//! Web-Mercator viewport camera.
//!
//! State: a centre point on Earth (`center_lonlat`, in WGS84 degrees) and
//! a **fractional** zoom (the OSM convention: zoom 0 = whole-world tile,
//! each integer step doubles the linear resolution).
//!
//! Public surface:
//! - [`Camera::pan`] — shift the centre by a pixel delta (the natural
//!   shape for a mouse drag).
//! - [`Camera::zoom_at`] — adjust zoom while keeping the world point
//!   under a given screen pixel pinned (the natural shape for a wheel
//!   zoom).
//! - [`Camera::visible_tiles`] — the integer XYZ tile addresses the
//!   current viewport covers, at the **rounded** integer zoom level.
//! - [`Camera::tile_ndc_rect`] — the screen-NDC rectangle a given tile
//!   occupies, used by the multi-tile renderer to position each quad.
//!
//! Convention reminder (see also [`crate::crs`]): every public function
//! takes `(lon, lat)` in that order. Screen pixels are in
//! `(x, y)` with `(0, 0)` at the top-left, `+y` down — the browser /
//! winit convention, not the OpenGL one.

use crate::crs;
use crate::tile::TileId;

/// Pixel size of one raster tile at its native zoom. The OSM /
/// Web-Mercator convention is 256.
pub const TILE_PIXELS: f64 = 256.0;

/// Minimum + maximum zoom levels the camera will accept. Bounded so
/// the fractional `zoom` never blows out the `f64`-safe range for tile
/// math, and so the tile selector doesn't ever ask for `z > 22` (where
/// OSM tiles don't exist and `f32` shader coords would overflow).
pub const MIN_ZOOM: f64 = 0.0;
pub const MAX_ZOOM: f64 = 19.0;

/// Zoom at which the flat → spherical projection transition begins
/// (zoom ≥ this is rendered fully flat). Above this, the slippy-map
/// is the Mercator view we've always shipped.
pub const GLOBE_FLAT_ZOOM: f64 = 5.0;

/// Zoom at which the transition completes (zoom ≤ this is rendered
/// as a full 3D globe). Between this and [`GLOBE_FLAT_ZOOM`], the
/// vertex shader interpolates between the two projections via a
/// smoothstep curve.
pub const GLOBE_FULL_ZOOM: f64 = 2.0;

/// Web-Mercator viewport camera.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// The WGS84 `(lon, lat)` at the centre of the viewport.
    pub center_lonlat: (f64, f64),
    /// Fractional zoom — `pixels_per_world = TILE_PIXELS * 2^zoom`.
    pub zoom: f64,
}

impl Camera {
    /// A new camera centred at `(lon, lat)` and `zoom`.
    pub fn new(lon: f64, lat: f64, zoom: f64) -> Camera {
        Camera {
            center_lonlat: (lon, lat),
            zoom: zoom.clamp(MIN_ZOOM, MAX_ZOOM),
        }
    }

    /// How many display pixels one normalised-Mercator unit covers at
    /// the current zoom. The slippy-map "1 tile = 256 px" relation
    /// drops straight out of `TILE_PIXELS * 2^zoom`.
    pub fn pixels_per_world(&self) -> f64 {
        TILE_PIXELS * 2.0_f64.powf(self.zoom)
    }

    /// The "globeness" parameter for the flat-to-sphere transition:
    /// **0.0** = render as flat Web Mercator (the slippy-map view);
    /// **1.0** = render as a 3D globe.
    ///
    /// Driven by zoom via a smoothstep: fully flat at
    /// `zoom ≥ GLOBE_FLAT_ZOOM`, fully globe at
    /// `zoom ≤ GLOBE_FULL_ZOOM`, smooth in between. The vertex
    /// shader interpolates the vertex position between the two
    /// projections by this value.
    pub fn globeness(&self) -> f32 {
        let t =
            ((GLOBE_FLAT_ZOOM - self.zoom) / (GLOBE_FLAT_ZOOM - GLOBE_FULL_ZOOM)).clamp(0.0, 1.0);
        // Smoothstep: 3t² - 2t³.
        (t * t * (3.0 - 2.0 * t)) as f32
    }

    /// Camera centre as `(lon_rad, lat_rad)` — what the sphere
    /// vertex shader needs to rotate the globe so the camera centre
    /// is at the "front" (positive Z after rotation).
    pub fn center_lonlat_rad(&self) -> [f32; 2] {
        [
            self.center_lonlat.0.to_radians() as f32,
            self.center_lonlat.1.to_radians() as f32,
        ]
    }

    /// Pan by a screen-pixel delta. The convention matches a mouse
    /// drag: `+dx` moves the user's view to the right, which means
    /// the world under the cursor moves to the right → the camera
    /// centre moves to the **left**. Same for y.
    ///
    /// At low zoom (globeness > 0) the pan rate is blended toward a
    /// **globe-aware** rate where dragging a full canvas width
    /// rotates the sphere by the visible-arc angle (~`2·asin(0.9)`
    /// = ~128° with the default `globe_scale`). Without this blend,
    /// pan uses `1/pixels_per_world` as the world-units-per-pixel
    /// scale, which is calibrated for the flat-Mercator view and
    /// makes the globe spin out of control at zoom 0 (~140° per
    /// 100 px of drag). The blend interpolates between the two
    /// rates by `globeness` so the feel stays continuous across the
    /// transition.
    pub fn pan(&mut self, dx_px: f64, dy_px: f64, canvas_px: (u32, u32)) {
        // Flat: 1 px = 1/ppw world units.
        let flat_units_per_px = 1.0 / self.pixels_per_world();
        // Globe: the sphere fills `globe_scale` of NDC's half-width
        // (== `GLOBE_SCALE` in render.rs). The visible arc per
        // canvas width is `2 * asin(globe_scale)` radians. A drag of
        // one canvas width should rotate the sphere by that arc.
        const GLOBE_SCALE: f64 = 0.9;
        let visible_arc_world = (GLOBE_SCALE.asin() * 2.0) / (2.0 * std::f64::consts::PI);
        let globe_units_per_px = visible_arc_world / canvas_px.0.max(1) as f64;
        let g = self.globeness() as f64;
        let units_per_px = flat_units_per_px * (1.0 - g) + globe_units_per_px * g;

        let (wx, wy) = crs::lonlat_to_world(self.center_lonlat.0, self.center_lonlat.1);
        let new_wx = (wx - dx_px * units_per_px).rem_euclid(1.0);
        let new_wy = (wy - dy_px * units_per_px).clamp(0.0, 1.0);
        self.center_lonlat = crs::world_to_lonlat(new_wx, new_wy);
    }

    /// Zoom by `delta` (positive = zoom in) while keeping the world
    /// point under screen pixel `cursor_px` pinned to that pixel.
    /// `canvas_size_px` is the current `(width, height)` in physical
    /// pixels.
    pub fn zoom_at(&mut self, delta: f64, cursor_px: (f64, f64), canvas_size_px: (u32, u32)) {
        let world_before = self.screen_to_world(cursor_px, canvas_size_px);
        self.zoom = (self.zoom + delta).clamp(MIN_ZOOM, MAX_ZOOM);
        let world_after = self.screen_to_world(cursor_px, canvas_size_px);
        // Shift the centre so `world_after` == `world_before` post-zoom.
        let (wcx, wcy) = crs::lonlat_to_world(self.center_lonlat.0, self.center_lonlat.1);
        let new_wcx = (wcx + world_before.0 - world_after.0).rem_euclid(1.0);
        let new_wcy = (wcy + world_before.1 - world_after.1).clamp(0.0, 1.0);
        self.center_lonlat = crs::world_to_lonlat(new_wcx, new_wcy);
    }

    /// Convert a screen pixel to a normalised-Mercator `(x, y)`.
    pub fn screen_to_world(&self, pixel: (f64, f64), canvas: (u32, u32)) -> (f64, f64) {
        let ppw = self.pixels_per_world();
        let (wcx, wcy) = crs::lonlat_to_world(self.center_lonlat.0, self.center_lonlat.1);
        let dx_px = pixel.0 - canvas.0 as f64 / 2.0;
        let dy_px = pixel.1 - canvas.1 as f64 / 2.0;
        (wcx + dx_px / ppw, wcy + dy_px / ppw)
    }

    /// The integer XYZ tiles the viewport currently covers, at zoom
    /// `round(self.zoom)`. Returned in row-major order (north→south,
    /// west→east within each row).
    ///
    /// **Limitation:** tile indices are clamped to `[0, n-1]`; the
    /// antimeridian-wrap case (where the viewport straddles ±180°
    /// longitude and the same tile should render at two screen
    /// positions) is not handled here and lands with the wrap-aware
    /// tile selector in a later milestone. For Chicago at zoom 10 this
    /// is moot — and at zoom 0 the entire world is one tile.
    pub fn visible_tiles(&self, canvas: (u32, u32)) -> Vec<TileId> {
        let z = self.zoom.round().clamp(MIN_ZOOM, MAX_ZOOM) as u8;
        let n = 1u32 << z;
        let n_f = n as f64;
        let max_i = (n - 1) as i64;
        let ppw = self.pixels_per_world();
        let (wcx, wcy) = crs::lonlat_to_world(self.center_lonlat.0, self.center_lonlat.1);
        let half_w_world = canvas.0 as f64 / 2.0 / ppw;
        let half_h_world = canvas.1 as f64 / 2.0 / ppw;
        let left = wcx - half_w_world;
        let right = wcx + half_w_world;
        let top = (wcy - half_h_world).max(0.0);
        let bottom = (wcy + half_h_world).min(1.0);

        let clamp = |v: f64| (v as i64).clamp(0, max_i);
        let tile_min_x = clamp((left * n_f).floor());
        let tile_max_x = clamp((right * n_f).floor());
        let tile_min_y = clamp((top * n_f).floor());
        let tile_max_y = clamp((bottom * n_f).floor());

        let mut tiles = Vec::new();
        for ty in tile_min_y..=tile_max_y {
            for tx in tile_min_x..=tile_max_x {
                tiles.push(TileId {
                    z,
                    x: tx as u32,
                    y: ty as u32,
                });
            }
        }
        tiles
    }

    /// True if the given tile's screen-NDC rect intersects the
    /// viewport. The renderer uses this for multi-zoom rendering:
    /// every loaded tile (at any zoom) whose rect overlaps `[-1, 1]²`
    /// gets drawn this frame, providing a coarser-or-finer fallback
    /// during pan + zoom transitions while the "right" tiles load.
    pub fn tile_visible(&self, tile: TileId, canvas: (u32, u32)) -> bool {
        let r = self.tile_ndc_rect(tile, canvas);
        // Standard AABB overlap with the unit square [-1, +1] × [-1, +1].
        r[2] > -1.0 && r[0] < 1.0 && r[3] > -1.0 && r[1] < 1.0
    }

    /// Screen-NDC rectangle `(x_min, y_min, x_max, y_max)` occupied by
    /// the given tile at the **current** (possibly fractional) zoom.
    /// NDC convention: `x` and `y` both `[-1, +1]` with `+y` up.
    pub fn tile_ndc_rect(&self, tile: TileId, canvas: (u32, u32)) -> [f32; 4] {
        let n = (1u32 << tile.z) as f64;
        let tile_world_min = (tile.x as f64 / n, tile.y as f64 / n);
        let tile_world_max = ((tile.x as f64 + 1.0) / n, (tile.y as f64 + 1.0) / n);

        let (wcx, wcy) = crs::lonlat_to_world(self.center_lonlat.0, self.center_lonlat.1);
        let ppw = self.pixels_per_world();
        let half_w_px = canvas.0 as f64 / 2.0;
        let half_h_px = canvas.1 as f64 / 2.0;

        // World → screen pixel (origin at canvas centre, +y down).
        let to_px_x = |wx: f64| (wx - wcx) * ppw;
        let to_px_y = |wy: f64| (wy - wcy) * ppw;
        // Pixel → NDC (flip y so +y is up).
        let to_ndc_x = |px: f64| (px / half_w_px) as f32;
        let to_ndc_y = |py: f64| -(py / half_h_px) as f32;

        let x_min = to_ndc_x(to_px_x(tile_world_min.0));
        let x_max = to_ndc_x(to_px_x(tile_world_max.0));
        // Mercator y increases southward; NDC y increases northward → swap.
        let y_min = to_ndc_y(to_px_y(tile_world_max.1));
        let y_max = to_ndc_y(to_px_y(tile_world_min.1));
        [x_min, y_min, x_max, y_max]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tile::CHICAGO_LONLAT;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    // -----------------------------------------------------------------
    // globeness — flat ↔ sphere transition curve
    // -----------------------------------------------------------------

    #[test]
    fn globeness_zero_at_high_zoom() {
        let c = Camera::new(0.0, 0.0, 10.0);
        assert_eq!(c.globeness(), 0.0);
        let c = Camera::new(0.0, 0.0, GLOBE_FLAT_ZOOM);
        assert_eq!(c.globeness(), 0.0);
    }

    #[test]
    fn globeness_one_at_low_zoom() {
        let c = Camera::new(0.0, 0.0, GLOBE_FULL_ZOOM);
        assert_eq!(c.globeness(), 1.0);
        let c = Camera::new(0.0, 0.0, 0.0);
        assert_eq!(c.globeness(), 1.0);
    }

    #[test]
    fn globeness_smooth_in_transition_band() {
        let mid_zoom = (GLOBE_FLAT_ZOOM + GLOBE_FULL_ZOOM) / 2.0;
        let c = Camera::new(0.0, 0.0, mid_zoom);
        // At the midpoint of the band, smoothstep gives 0.5 exactly.
        assert!((c.globeness() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn globeness_monotonic_in_transition_band() {
        // Sweep across the transition and confirm globeness is
        // monotonically non-increasing in zoom (higher zoom = less
        // globey).
        let mut last = 1.0_f32;
        let steps = 64;
        for i in 0..=steps {
            let t = i as f64 / steps as f64;
            let zoom = GLOBE_FULL_ZOOM + t * (GLOBE_FLAT_ZOOM - GLOBE_FULL_ZOOM);
            let g = Camera::new(0.0, 0.0, zoom).globeness();
            assert!(
                g <= last + 1e-6,
                "globeness({zoom}) = {g} exceeded prior {last}"
            );
            last = g;
        }
    }

    #[test]
    fn center_lonlat_rad_converts_to_radians() {
        let c = Camera::new(180.0, 90.0, 0.0);
        let [lon, lat] = c.center_lonlat_rad();
        assert!((lon - std::f32::consts::PI).abs() < 1e-6);
        assert!((lat - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
    }

    // -----------------------------------------------------------------
    // Pre-existing zoom / pan / tile-visibility coverage
    // -----------------------------------------------------------------

    #[test]
    fn pixels_per_world_doubles_per_zoom() {
        let c = Camera::new(0.0, 0.0, 0.0);
        let ppw0 = c.pixels_per_world();
        assert!(close(ppw0, TILE_PIXELS, 1e-9));
        let c = Camera::new(0.0, 0.0, 1.0);
        assert!(close(c.pixels_per_world(), 2.0 * TILE_PIXELS, 1e-9));
        let c = Camera::new(0.0, 0.0, 10.0);
        assert!(close(c.pixels_per_world(), 1024.0 * TILE_PIXELS, 1e-9));
    }

    #[test]
    fn flat_pan_inverse_of_screen_motion() {
        // **Flat-regime invariant.** Drag the cursor 100 pixels right
        // and 50 pixels down at fully-flat zoom. The world point that
        // was under the cursor should now be 100 right and 50 down in
        // screen space — i.e. the centre moved 100 left / 50 up in
        // world space.
        //
        // At low zoom (globeness > 0) pan switches to a globe-aware
        // rate (different units-per-pixel) where this exact-inverse
        // invariant doesn't hold; see `globe_pan_rate_matches_visible_arc`.
        let mut c = Camera::new(0.0, 0.0, 10.0); // flat
        assert_eq!(c.globeness(), 0.0);
        let canvas = (512, 512);
        let world_before_center = c.screen_to_world((256.0, 256.0), canvas);
        c.pan(100.0, 50.0, canvas);
        let world_after_center_was_offset =
            c.screen_to_world((256.0 + 100.0, 256.0 + 50.0), canvas);
        assert!(
            close(world_before_center.0, world_after_center_was_offset.0, 1e-9),
            "pan x: {} vs {}",
            world_before_center.0,
            world_after_center_was_offset.0
        );
        assert!(
            close(world_before_center.1, world_after_center_was_offset.1, 1e-9),
            "pan y: {} vs {}",
            world_before_center.1,
            world_after_center_was_offset.1
        );
    }

    #[test]
    fn globe_pan_rate_matches_visible_arc() {
        // **Globe-regime invariant.** At full globeness, dragging a
        // full canvas width should rotate the sphere by the visible
        // arc angle (~`2·asin(0.9)` ≈ 128° with the default
        // globe_scale). Catches drift in the pan-rate constants the
        // shader's globe projection depends on.
        let mut c = Camera::new(0.0, 0.0, 0.0); // fully globe
        assert_eq!(c.globeness(), 1.0);
        let canvas = (1000, 1000);
        let lon_before = c.center_lonlat.0;
        c.pan(canvas.0 as f64, 0.0, canvas);
        let lon_after = c.center_lonlat.0;
        // Pan is "drag right → centre moves left", so the change is
        // negative; take abs.
        let dlon = (lon_after - lon_before).abs();
        // Expected: 2 · asin(0.9) radians, in degrees.
        let expected_deg = (0.9_f64.asin() * 2.0).to_degrees();
        assert!(
            (dlon - expected_deg).abs() < 0.5,
            "globe pan: dragged {} px → {}° rotation, expected ~{}°",
            canvas.0,
            dlon,
            expected_deg,
        );
    }

    #[test]
    fn zoom_at_pins_world_under_cursor() {
        // The world point under a given screen pixel must be the same
        // before and after zoom_at — that's the entire UX promise of
        // wheel-zoom-around-cursor.
        let mut c = Camera::new(CHICAGO_LONLAT.0, CHICAGO_LONLAT.1, 4.0);
        let canvas = (800, 600);
        let cursor = (640.0, 200.0); // somewhere off-centre
        let world_before = c.screen_to_world(cursor, canvas);
        c.zoom_at(1.7, cursor, canvas);
        let world_after = c.screen_to_world(cursor, canvas);
        assert!(
            close(world_before.0, world_after.0, 1e-9),
            "world x drift: {} → {}",
            world_before.0,
            world_after.0
        );
        assert!(
            close(world_before.1, world_after.1, 1e-9),
            "world y drift: {} → {}",
            world_before.1,
            world_after.1
        );
    }

    #[test]
    fn zoom_clamps_at_extremes() {
        let mut c = Camera::new(0.0, 0.0, MAX_ZOOM);
        c.zoom_at(5.0, (100.0, 100.0), (200, 200));
        assert!(close(c.zoom, MAX_ZOOM, 1e-12));

        let mut c = Camera::new(0.0, 0.0, MIN_ZOOM);
        c.zoom_at(-5.0, (100.0, 100.0), (200, 200));
        assert!(close(c.zoom, MIN_ZOOM, 1e-12));
    }

    #[test]
    fn visible_tiles_z0_is_single_tile() {
        let c = Camera::new(0.0, 0.0, 0.0);
        let tiles = c.visible_tiles((256, 256));
        assert_eq!(tiles, vec![TileId { z: 0, x: 0, y: 0 }]);
    }

    #[test]
    fn visible_tiles_z10_chicago_includes_chicago_tile() {
        // At zoom 10, Chicago (10, 262, 380) must be in the visible
        // set — that's the "first tile we wired up" sanity.
        let c = Camera::new(CHICAGO_LONLAT.0, CHICAGO_LONLAT.1, 10.0);
        let tiles = c.visible_tiles((800, 600));
        let chicago = TileId {
            z: 10,
            x: 262,
            y: 380,
        };
        assert!(
            tiles.contains(&chicago),
            "Chicago tile not in visible set: {tiles:?}"
        );
        // 800×600 viewport at z=10 with 256-px tiles covers about
        // 3-4 tiles wide × 3 tall — somewhere in [4, 16] inclusive.
        assert!(
            (4..=16).contains(&tiles.len()),
            "unexpected visible-tile count at z=10 / 800x600: {}",
            tiles.len()
        );
    }

    #[test]
    fn tile_visible_matches_visible_tiles_at_native_zoom() {
        // At the camera's exact zoom level, `tile_visible` should
        // agree with `visible_tiles` for every tile in the world.
        let c = Camera::new(CHICAGO_LONLAT.0, CHICAGO_LONLAT.1, 10.0);
        let canvas = (800, 600);
        let visible: std::collections::HashSet<_> = c.visible_tiles(canvas).into_iter().collect();
        for x in 256..272 {
            for y in 376..386 {
                let id = TileId { z: 10, x, y };
                assert_eq!(
                    c.tile_visible(id, canvas),
                    visible.contains(&id),
                    "z=10 tile ({x}, {y}): visible_tiles vs tile_visible disagree"
                );
            }
        }
    }

    #[test]
    fn tile_visible_includes_parent_zoom_during_zoom_in() {
        // At zoom 10.7 (between integer levels), the renderer picks
        // z=11 tiles via visible_tiles — but a z=10 tile covering the
        // same area still tests as visible. That's the multi-zoom
        // fallback the renderer relies on while z=11 tiles fetch.
        let c = Camera::new(CHICAGO_LONLAT.0, CHICAGO_LONLAT.1, 10.7);
        let canvas = (800, 600);
        let chicago_z10 = TileId {
            z: 10,
            x: 262,
            y: 380,
        };
        assert!(c.tile_visible(chicago_z10, canvas));
    }

    #[test]
    fn tile_visible_rejects_offscreen_tiles() {
        // A tile on the other side of the world should not be visible.
        let c = Camera::new(CHICAGO_LONLAT.0, CHICAGO_LONLAT.1, 10.0);
        let canvas = (800, 600);
        let antipodes = TileId {
            z: 10,
            x: 774,
            y: 380,
        }; // ~180° from Chicago
        assert!(!c.tile_visible(antipodes, canvas));
    }

    #[test]
    fn tile_ndc_rect_centre_tile_centred() {
        // Centre the camera exactly on the centre of tile (10, 262,
        // 380). The tile's NDC rect should be symmetric around the
        // origin and exactly 256 px wide on screen.
        let n = 1u32 << 10;
        let centre_world = ((262.0 + 0.5) / n as f64, (380.0 + 0.5) / n as f64);
        let (lon, lat) = crs::world_to_lonlat(centre_world.0, centre_world.1);
        let c = Camera::new(lon, lat, 10.0);
        let canvas = (800, 600);
        let rect = c.tile_ndc_rect(
            TileId {
                z: 10,
                x: 262,
                y: 380,
            },
            canvas,
        );
        // Rect is symmetric around origin.
        assert!(
            close(rect[0] as f64, -rect[2] as f64, 1e-6),
            "x sym: {rect:?}"
        );
        assert!(
            close(rect[1] as f64, -rect[3] as f64, 1e-6),
            "y sym: {rect:?}"
        );
        // Width in NDC = (256 px) / (400 px half-width) = 0.64.
        assert!(close(rect[2] as f64 - rect[0] as f64, 256.0 / 400.0, 1e-6));
        // Height in NDC = (256 px) / (300 px half-height) ≈ 0.853.
        assert!(close(rect[3] as f64 - rect[1] as f64, 256.0 / 300.0, 1e-6));
    }
}
