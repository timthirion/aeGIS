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
use crate::tile::{self, TileId, TileProjection};

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
    /// Override of the default lower zoom bound. Defaults to
    /// [`MIN_ZOOM`] (`0.0`); the renderer drops this to a negative
    /// value when satellites are currently being rendered, so the
    /// user can pull back far enough to see GNSS (~3.2 Earth radii
    /// above surface) and geostationary (~5.6 Earth radii) orbits.
    /// When all satellites are hidden / no category is enabled, it
    /// resets to [`MIN_ZOOM`].
    pub min_zoom: f64,
}

impl Camera {
    /// A new camera centred at `(lon, lat)` and `zoom`.
    pub fn new(lon: f64, lat: f64, zoom: f64) -> Camera {
        Camera {
            center_lonlat: (lon, lat),
            zoom: zoom.clamp(MIN_ZOOM, MAX_ZOOM),
            min_zoom: MIN_ZOOM,
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

    /// 3D position of the perspective camera in the same coordinate
    /// system as `lonlat_to_sphere` returns. Camera sits on the line
    /// from sphere centre outward through the camera's `(lon, lat)`,
    /// at a distance `D = 1 + altitude(zoom, canvas)`.
    pub fn camera_3d_position(&self, canvas: (u32, u32)) -> [f32; 3] {
        let lon = self.center_lonlat.0.to_radians();
        let lat = self.center_lonlat.1.to_radians();
        let c = [
            (lat.cos() * lon.sin()) as f32,
            lat.sin() as f32,
            (lat.cos() * lon.cos()) as f32,
        ];
        let d = 1.0 + self.altitude(canvas) as f32;
        [c[0] * d, c[1] * d, c[2] * d]
    }

    /// Camera altitude above the sphere surface in 3D units (sphere
    /// radius = 1). Computed to match the flat-Mercator slippy-map
    /// scale at high zoom, capped at the low end so the sphere stays
    /// visible at zoom 0 rather than shrinking to a dot.
    ///
    /// At high zoom (z ≥ ~4), altitude follows
    /// `H · π / (256 · 2^z · tan(fov/2))` so 1 world unit on the
    /// sphere subtends the same screen pixels as the slippy map
    /// would. At low zoom, altitude clamps to `ALTITUDE_CEIL` so the
    /// globe stays ~60% of the canvas.
    pub fn altitude(&self, canvas: (u32, u32)) -> f64 {
        const FOV_Y_RAD: f64 = std::f64::consts::FRAC_PI_3; // 60°
        const ALTITUDE_CEIL: f64 = 2.0; // D ≤ 3 → globe subtends ~38°
                                        // Below z=0 the camera pulls back linearly so the user
                                        // can frame high-altitude orbits (GNSS, geostationary)
                                        // when satellites are visible. 2 extra Earth radii per
                                        // zoom step → z = −1 gives D = 5, z = −2 gives D = 7
                                        // (GNSS visible), z = −3 gives D = 9 (geostationary).
                                        // The default `min_zoom = 0.0` keeps this branch
                                        // unreachable unless the renderer has explicitly lowered
                                        // the bound.
        if self.zoom < 0.0 {
            return ALTITUDE_CEIL + (-self.zoom) * 2.0;
        }
        let slippy = canvas.1 as f64 * std::f64::consts::PI
            / (256.0 * 2.0_f64.powf(self.zoom) * (FOV_Y_RAD * 0.5).tan());
        slippy.min(ALTITUDE_CEIL)
    }

    /// Combined view × perspective-projection 4x4 matrix in
    /// **column-major** order (the convention `wgpu` / WGSL's
    /// `mat4x4<f32>` reads).
    ///
    /// The `near` plane scales with altitude (~10% of the altitude,
    /// floored at `1e-6`) so the sphere surface is always inside the
    /// visible depth range. A fixed `near` would clip the entire
    /// scene at zoom ≥ ~12, when the camera-to-surface distance drops
    /// below the fixed value and everything visible falls behind the
    /// near plane.
    pub fn view_projection_matrix(&self, canvas: (u32, u32)) -> [f32; 16] {
        let cam_pos = self.camera_3d_position(canvas);
        let aspect = canvas.0 as f32 / canvas.1.max(1) as f32;
        // "Up" handling near the poles: when the camera is nearly
        // along the +Y axis (overhead view), the canonical +Y up
        // would be parallel to the look direction. Pick an up vector
        // that always has a usable tangent component.
        let up = if self.center_lonlat.1.abs() > 89.0 {
            // Looking nearly straight down (or up) — use +Z as up so
            // the look direction (+Y or -Y) crosses it cleanly.
            [0.0, 0.0, 1.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        let view = look_at(cam_pos, [0.0, 0.0, 0.0], up);
        let altitude = self.altitude(canvas) as f32;
        let near = (altitude * 0.1).max(1e-6);
        // far must comfortably contain the far side of the sphere
        // (camera-to-far-vertex distance = D + 1 = altitude + 2).
        let far = (altitude + 2.0).max(10.0) * 1.5;
        let proj = perspective(60.0_f32.to_radians(), aspect, near, far);
        mat4_mul(proj, view)
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

    /// Zoom by `delta` (positive = zoom in). At flat-Mercator zoom
    /// the world point under screen pixel `cursor_px` stays pinned
    /// to that pixel (the natural shape for wheel-zoom-around-cursor).
    /// At globe view the pinning fades out so a wheel tick is a pure
    /// zoom around the camera centre. `canvas_size_px` is the current
    /// `(width, height)` in physical pixels.
    ///
    /// **Why the fade.** `screen_to_world` uses flat-Mercator math.
    /// At low zoom the canvas covers way more than one world width
    /// of normalised Mercator (ppw = 256 at zoom 0, half-canvas at
    /// 500 px → world half-width 1.95). The cursor at any
    /// non-centre position maps to a "world coord" outside [0, 1],
    /// which doesn't correspond to anything the user sees on the
    /// 3D-projected sphere. Pinning that phantom point translates
    /// to the camera rotating a non-trivial arc per wheel tick —
    /// "the ball wants to roll above all else." Blending the shift
    /// by `(1 - globeness)` collapses the rotation to zero at full
    /// globe and re-introduces it smoothly across the transition
    /// band as the slippy-map regime takes over.
    pub fn zoom_at(&mut self, delta: f64, cursor_px: (f64, f64), canvas_size_px: (u32, u32)) {
        let world_before = self.screen_to_world(cursor_px, canvas_size_px);
        self.zoom = (self.zoom + delta).clamp(self.min_zoom, MAX_ZOOM);
        let world_after = self.screen_to_world(cursor_px, canvas_size_px);
        // Shift the centre so `world_after` == `world_before`, scaled
        // by `(1 - globeness)` so the pinning fades out as the camera
        // approaches the full globe view.
        let pin = 1.0 - self.globeness() as f64;
        let shift_x = (world_before.0 - world_after.0) * pin;
        let shift_y = (world_before.1 - world_after.1) * pin;
        let (wcx, wcy) = crs::lonlat_to_world(self.center_lonlat.0, self.center_lonlat.1);
        let new_wcx = (wcx + shift_x).rem_euclid(1.0);
        let new_wcy = (wcy + shift_y).clamp(0.0, 1.0);
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
        // Legacy entry point that assumes WebMercator. Tests + the
        // Earth Carto dispatch use this; multi-projection callers go
        // straight to `visible_tiles_capped` with an explicit
        // `TileProjection`.
        self.visible_tiles_capped(canvas, MAX_ZOOM as u8, TileProjection::WebMercator)
    }

    /// Like [`Self::visible_tiles`] but clamps the chosen zoom to
    /// `max_z` and accepts the basemap's tile-grid `projection`.
    /// Used by providers whose pyramid stops short of `MAX_ZOOM`
    /// (Esri caps at z=19; NASA Trek caps lower per layer). Past
    /// the cap the camera keeps zooming but tile resolution stays
    /// at the cap — the imagery just renders larger.
    pub fn visible_tiles_capped(
        &self,
        canvas: (u32, u32),
        max_z: u8,
        projection: TileProjection,
    ) -> Vec<TileId> {
        let max_z_f = (max_z as f64).min(MAX_ZOOM);
        let z = self.zoom.round().clamp(MIN_ZOOM, max_z_f) as u8;

        // On the globe (and during the flat ↔ globe transition), the
        // flat-Mercator viewport rect under-represents what the camera
        // actually sees. At zoom 3 the camera is looking at a sphere
        // and a third of the world is in view, but flat math would
        // return a 4-tile-wide strip near the centre. Switch to a
        // sphere-cap test whenever any curvature is being rendered.
        if self.globeness() > 0.0 {
            return self.visible_tiles_globe(canvas, z, projection);
        }

        let n_x = tile::tile_grid_width(projection, z);
        let n_y = tile::tile_grid_height(z);
        let n_x_f = n_x as f64;
        let n_y_f = n_y as f64;
        let max_x_i = (n_x - 1) as i64;
        let max_y_i = (n_y - 1) as i64;
        let ppw = self.pixels_per_world();
        // The viewport rect in projection-native world coords. For
        // WebMercator we use the existing `crs::lonlat_to_world`
        // (Mercator-y stretched). For Equirectangular the world is
        // linear in lat: `world_y = (90 - lat) / 180`.
        let (left, right, top, bottom) = match projection {
            TileProjection::WebMercator => {
                let (wcx, wcy) = crs::lonlat_to_world(self.center_lonlat.0, self.center_lonlat.1);
                let half_w = canvas.0 as f64 / 2.0 / ppw;
                let half_h = canvas.1 as f64 / 2.0 / ppw;
                (
                    wcx - half_w,
                    wcx + half_w,
                    (wcy - half_h).max(0.0),
                    (wcy + half_h).min(1.0),
                )
            }
            TileProjection::Equirectangular => {
                // Treat one "world unit" the same in both projections
                // (so the camera's pan/zoom scale stays consistent).
                // The Equirectangular grid is twice as wide in tile
                // count, so a tile is half as wide in world units —
                // the `n_x = 2*2^z` already encodes that.
                let lon = self.center_lonlat.0;
                let lat = self.center_lonlat.1;
                let eq_cx = (lon + 180.0) / 360.0;
                let eq_cy = (90.0 - lat) / 180.0;
                let half_w = canvas.0 as f64 / 2.0 / ppw;
                let half_h = canvas.1 as f64 / 2.0 / ppw;
                (
                    eq_cx - half_w,
                    eq_cx + half_w,
                    (eq_cy - half_h).max(0.0),
                    (eq_cy + half_h).min(1.0),
                )
            }
        };

        // Two-tile margin around the floored rect (see initial-load
        // rim fix in plan 0002 epilogue).
        let clamp_x = |v: f64| (v as i64).clamp(0, max_x_i);
        let clamp_y = |v: f64| (v as i64).clamp(0, max_y_i);
        let tile_min_x = clamp_x((left * n_x_f).floor() - 2.0);
        let tile_max_x = clamp_x((right * n_x_f).floor() + 2.0);
        let tile_min_y = clamp_y((top * n_y_f).floor() - 2.0);
        let tile_max_y = clamp_y((bottom * n_y_f).floor() + 2.0);

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

    /// Tiles at zoom `z` whose footprint touches the camera's
    /// front-facing spherical cap. Used by [`Self::visible_tiles_capped`]
    /// at low zoom where the slippy-map rectangle is the wrong shape.
    ///
    /// Cap geometry: with the sphere at unit radius and the camera at
    /// distance `D = 1 + altitude`, a surface point P is visible iff
    /// `P · C ≥ 1/D` (both as unit vectors from the sphere centre).
    ///
    /// We sample nine points per tile (centre + 4 corners + 4 edge
    /// midpoints) and include the tile if any sample passes the cap
    /// test. A centre-only check works at high zoom where tiles span
    /// fractions of a degree, but at z=2 a tile spans 90° of longitude
    /// — its centre can be well behind the limb while a corner is
    /// firmly in view, and the centre test silently drops it. A small
    /// margin past the strict limb absorbs sub-tile slivers near the
    /// horizon.
    fn visible_tiles_globe(
        &self,
        canvas: (u32, u32),
        z: u8,
        projection: TileProjection,
    ) -> Vec<TileId> {
        let n_x = tile::tile_grid_width(projection, z);
        let n_y = tile::tile_grid_height(z);
        let n_x_f = n_x as f64;
        let n_y_f = n_y as f64;
        let lon_c = self.center_lonlat.0.to_radians();
        let lat_c = self.center_lonlat.1.to_radians();
        let cam_dir = [
            lat_c.cos() * lon_c.sin(),
            lat_c.sin(),
            lat_c.cos() * lon_c.cos(),
        ];
        let d = 1.0 + self.altitude(canvas);
        let limb_cos = (1.0 / d) - 0.05;

        // Tile-local sample positions in `[0, 1]²` — centre, then the
        // four corners, then the four edge midpoints.
        const SAMPLES: [(f64, f64); 9] = [
            (0.5, 0.5),
            (0.0, 0.0),
            (1.0, 0.0),
            (0.0, 1.0),
            (1.0, 1.0),
            (0.5, 0.0),
            (0.5, 1.0),
            (0.0, 0.5),
            (1.0, 0.5),
        ];

        let mut tiles = Vec::new();
        for ty in 0..n_y {
            for tx in 0..n_x {
                let mut any_in_cap = false;
                for &(sx, sy) in &SAMPLES {
                    let (lon, lat) = match projection {
                        TileProjection::WebMercator => {
                            let wx = (tx as f64 + sx) / n_x_f;
                            let wy = (ty as f64 + sy) / n_y_f;
                            crs::world_to_lonlat(wx, wy)
                        }
                        TileProjection::Equirectangular => {
                            let wx = (tx as f64 + sx) / n_x_f;
                            let wy = (ty as f64 + sy) / n_y_f;
                            // Linear: wx ∈ [0,1] → lon ∈ [-180, 180];
                            // wy ∈ [0,1] → lat ∈ [+90, -90].
                            (wx * 360.0 - 180.0, 90.0 - wy * 180.0)
                        }
                    };
                    let lon_r = lon.to_radians();
                    let lat_r = lat.to_radians();
                    let p = [
                        lat_r.cos() * lon_r.sin(),
                        lat_r.sin(),
                        lat_r.cos() * lon_r.cos(),
                    ];
                    let dot = p[0] * cam_dir[0] + p[1] * cam_dir[1] + p[2] * cam_dir[2];
                    if dot > limb_cos {
                        any_in_cap = true;
                        break;
                    }
                }
                if any_in_cap {
                    tiles.push(TileId { z, x: tx, y: ty });
                }
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

// ---------------------------------------------------------------------------
// Small 4x4 matrix helpers — inlined to avoid pulling in a math crate.
// Column-major throughout (the WGSL `mat4x4<f32>` convention).
// ---------------------------------------------------------------------------

/// Right-handed perspective projection matrix for the **wgpu /
/// Vulkan / DirectX clip-space convention** (depth in `[0, 1]`,
/// not OpenGL's `[-1, +1]`). Maps `+y up`, looking down `-z`, with
/// `near` and `far` as positive distances.
///
/// Critical for wgpu: using the OpenGL convention here would map
/// near-plane vertices to clip-z = -1, outside wgpu's valid range,
/// so the whole foreground would get clipped.
fn perspective(fov_y_rad: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    let f = 1.0 / (fov_y_rad / 2.0).tan();
    let nf = 1.0 / (near - far);
    let mut m = [0.0_f32; 16];
    m[0] = f / aspect;
    m[5] = f;
    m[10] = far * nf;
    m[11] = -1.0;
    m[14] = near * far * nf;
    m
}

/// Right-handed look-at view matrix.
fn look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> [f32; 16] {
    let f = normalize([target[0] - eye[0], target[1] - eye[1], target[2] - eye[2]]);
    let s = normalize(cross(f, up));
    let u = cross(s, f);
    let mut m = [0.0_f32; 16];
    m[0] = s[0];
    m[1] = u[0];
    m[2] = -f[0];
    m[3] = 0.0;
    m[4] = s[1];
    m[5] = u[1];
    m[6] = -f[1];
    m[7] = 0.0;
    m[8] = s[2];
    m[9] = u[2];
    m[10] = -f[2];
    m[11] = 0.0;
    m[12] = -dot(s, eye);
    m[13] = -dot(u, eye);
    m[14] = dot(f, eye);
    m[15] = 1.0;
    m
}

/// Column-major 4x4 multiply: `m = a * b`.
fn mat4_mul(a: [f32; 16], b: [f32; 16]) -> [f32; 16] {
    let mut m = [0.0_f32; 16];
    for col in 0..4 {
        for row in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k * 4 + row] * b[col * 4 + k];
            }
            m[col * 4 + row] = sum;
        }
    }
    m
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 0.0 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        v
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
    fn view_projection_visible_at_high_zoom() {
        // Regression test for "high zoom garbles the map" — caused
        // by (a) OpenGL-convention perspective matrix and (b)
        // hardcoded near=0.01 plane clipping the sphere surface
        // once altitude dropped below 0.01 (zoom ≥ 12).
        //
        // For each test zoom: project the sphere point under the
        // camera (which sits at `camera_3d_position`, pointing at
        // the sphere centre) and confirm it lands inside the wgpu
        // clip-space cube (x, y, z ∈ [-1, 1] for x/y and [0, 1] for z).
        let canvas = (1000_u32, 1000_u32);
        for &zoom in &[0.0, 2.0, 5.0, 10.0, 14.0, 18.0] {
            let cam = Camera::new(0.0, 0.0, zoom);
            let m = cam.view_projection_matrix(canvas);
            // Surface point at camera centre = (0, 0, 1) for cam at
            // (lon=0, lat=0) in our sphere convention.
            let p = [0.0_f32, 0.0, 1.0, 1.0];
            // Column-major mat * vec.
            let mut clip = [0.0_f32; 4];
            for row in 0..4 {
                let mut s = 0.0;
                for col in 0..4 {
                    s += m[col * 4 + row] * p[col];
                }
                clip[row] = s;
            }
            let z = clip[2] / clip[3];
            assert!(
                (0.0..=1.0).contains(&z),
                "z={zoom}: camera-centre sphere point projects to clip-z {z:?} (must be in [0, 1] for wgpu)"
            );
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
    fn zoom_at_pins_world_under_cursor_at_flat_zoom() {
        // **Flat-regime invariant.** The world point under a given
        // screen pixel must be the same before and after zoom_at —
        // that's the UX promise of wheel-zoom-around-cursor in the
        // slippy-map regime. At globe-transition zooms the pinning
        // intentionally fades out; see the globe-view test below.
        let mut c = Camera::new(CHICAGO_LONLAT.0, CHICAGO_LONLAT.1, 10.0);
        assert_eq!(c.globeness(), 0.0);
        let canvas = (800, 600);
        let cursor = (640.0, 200.0); // off-centre
        let world_before = c.screen_to_world(cursor, canvas);
        c.zoom_at(0.5, cursor, canvas);
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
    fn zoom_at_does_not_pan_at_full_globe() {
        // **Globe-regime invariant.** At globeness=1 a wheel tick must
        // be a pure zoom around the camera centre. The previous
        // behaviour pinned the cursor world point computed from
        // flat-Mercator math, which at globe-scale maps off-centre
        // cursors to world coords well outside [0, 1] and produced
        // a meaningful camera rotation per wheel tick — making
        // initial zoom-in from the globe view hard to control.
        let mut c = Camera::new(CHICAGO_LONLAT.0, CHICAGO_LONLAT.1, 0.0);
        assert_eq!(c.globeness(), 1.0);
        let centre_before = c.center_lonlat;
        // A maximally-off-centre cursor at full globe — anywhere this
        // mapping breaks down the worst.
        c.zoom_at(0.5, (790.0, 10.0), (800, 600));
        let centre_after = c.center_lonlat;
        // Centre survives the forward+inverse Mercator round trip in
        // zoom_at to within 1e-9; allow 1e-6 for inherited slop.
        assert!(
            close(centre_before.0, centre_after.0, 1e-6),
            "lon shift at globe view: {} → {}",
            centre_before.0,
            centre_after.0
        );
        assert!(
            close(centre_before.1, centre_after.1, 1e-6),
            "lat shift at globe view: {} → {}",
            centre_before.1,
            centre_after.1
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
        // 3-4 tiles wide × 3 tall, plus a two-tile margin on every
        // side (so the rim is always dispatched) — about 7-8 × 6-7
        // ≈ 42-56, with [24, 80] as a generous outer bound that
        // won't flap if the Chicago coords nudge.
        assert!(
            (24..=80).contains(&tiles.len()),
            "unexpected visible-tile count at z=10 / 800x600: {}",
            tiles.len()
        );
    }

    #[test]
    fn visible_tiles_on_globe_includes_corner_visible_tiles() {
        // At z=2 each tile spans 90° of longitude and ~30° of
        // latitude — large enough that a tile centre can sit past
        // the limb while a corner is still firmly in view. From a
        // Chicago camera, tile (z=2, x=2, y=1) has its centre at
        // (lon=45°, lat=41°) — outside the visible cap at altitude=2
        // — but its west edge (lon=0°, lat=41°) is well inside. The
        // dispatcher must include it; the centre-only check this
        // method previously used silently dropped it.
        let c = Camera::new(CHICAGO_LONLAT.0, CHICAGO_LONLAT.1, 2.0);
        let tiles = c.visible_tiles((800, 600));
        let corner_visible = TileId { z: 2, x: 2, y: 1 };
        assert!(
            tiles.contains(&corner_visible),
            "z=2 tile with corner in view but centre past limb was dropped: {tiles:?}"
        );
    }

    #[test]
    fn visible_tiles_on_globe_covers_hemisphere() {
        // At zoom 3 the renderer is mostly globe (globeness ~0.74)
        // and the camera can see roughly a third of the sphere. Flat-
        // viewport math would return a 4-tile-wide strip near the
        // camera centre (~12 tiles); the sphere-cap path should
        // return many more — and must include tiles well outside
        // the flat rect, e.g. 90° east of Chicago.
        let c = Camera::new(CHICAGO_LONLAT.0, CHICAGO_LONLAT.1, 3.0);
        let tiles = c.visible_tiles((800, 600));
        assert!(
            tiles.len() > 20,
            "globe-view z=3 should reveal more than a flat strip: got {} tiles",
            tiles.len()
        );
        // Chicago is roughly (z=3, x=2, y=2); 90° east puts us around
        // x=4 at the same row — well outside the flat-Mercator rect
        // centred on Chicago but solidly on the visible hemisphere.
        let far_east_same_row = TileId { z: 3, x: 4, y: 2 };
        assert!(
            tiles.contains(&far_east_same_row),
            "globe-view z=3 should include a tile 90° east of camera: {tiles:?}"
        );
    }

    #[test]
    fn tile_visible_is_subset_of_visible_tiles_at_native_zoom() {
        // Every tile that `tile_visible` accepts (strict NDC-rect
        // overlap with the viewport) must also be in `visible_tiles`
        // (which dispatches with a one-tile edge margin). The reverse
        // does NOT hold: the margin tiles aren't strictly visible.
        let c = Camera::new(CHICAGO_LONLAT.0, CHICAGO_LONLAT.1, 10.0);
        let canvas = (800, 600);
        let visible: std::collections::HashSet<_> = c.visible_tiles(canvas).into_iter().collect();
        for x in 256..272 {
            for y in 376..386 {
                let id = TileId { z: 10, x, y };
                if c.tile_visible(id, canvas) {
                    assert!(
                        visible.contains(&id),
                        "z=10 tile ({x}, {y}): in viewport but not dispatched"
                    );
                }
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
