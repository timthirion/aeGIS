//! Camera fly-to: smooth animation from the current camera state
//! to a target `(lon, lat, zoom)`, or to a target bounding box.
//!
//! The renderer holds an `Option<FlyTo>` and ticks it from the
//! frame loop; on every tick the camera's `(center_lonlat, zoom)`
//! are overwritten by `flyto::sample(state, now)`. When `t >= 1`
//! the state clears.
//!
//! ### Position interpolation: slerp, not lon/lat lerp
//!
//! Linearly interpolating `(lon, lat)` is a rhumb line in lon/lat
//! space — not a great circle on the sphere. Reykjavík → Sydney
//! via lon-lerp crosses the equator; the actual great-circle
//! path goes over the south pole. We slerp on the unit sphere
//! instead:
//!
//! ```text
//! Ω = acos(p0 · p1)
//! p(t) = (sin((1-t)·Ω) · p0 + sin(t·Ω) · p1) / sin(Ω)
//! ```
//!
//! Then re-derive `(lon, lat)` from `p(t)` via the inverse of the
//! sphere convention the renderer already uses (`p =
//! (cos(lat)·sin(lon), sin(lat), cos(lat)·cos(lon))`).
//!
//! ### Zoom interpolation: out then in
//!
//! For long flies, easing zoom monotonically from start to target
//! reads as "the world is sliding under me" — disorienting. The
//! convention every interactive map uses is **zoom out then zoom
//! back in**: pull back, glide across, drop down. We do the same:
//! the first half eases from `start_zoom` down to an
//! `intermediate_zoom` that scales with the great-circle distance;
//! the second half eases from `intermediate_zoom` to `target_zoom`.

use crate::camera::{self, Camera};
use crate::crs;

/// In-flight fly-to animation state.
#[derive(Copy, Clone, Debug)]
pub struct FlyTo {
    pub start_lonlat: (f64, f64),
    pub start_zoom: f64,
    pub target_lonlat: (f64, f64),
    pub target_zoom: f64,
    /// Monotonic time (seconds) the animation started. The renderer
    /// supplies "now" from `web_sys::Performance::now()` on web and
    /// `std::time::Instant` on native; the only contract is that
    /// `now() - started_at` increases monotonically and is in
    /// seconds.
    pub started_at: f64,
    /// Total animation duration in seconds. Derived from the
    /// great-circle arc length at construction.
    pub duration: f64,
}

/// Minimum fly-to duration in seconds. Sub-degree flies snap fast
/// so a coord-paste feels responsive.
pub const MIN_DURATION_S: f64 = 0.4;

/// Maximum fly-to duration in seconds. Antipodal flies get the
/// long ease so the "zoom out across the globe" feels intentional
/// rather than rushed.
pub const MAX_DURATION_S: f64 = 2.0;

/// At zoom 1.5 the camera is fully in globe view (per
/// `camera::GLOBE_FULL_ZOOM = 2.0`). Long flies drop the
/// intermediate zoom to this floor.
pub const GLOBE_VIEW_ZOOM: f64 = 1.5;

impl FlyTo {
    /// Construct a fly-to from the current camera to
    /// `(target_lonlat, target_zoom)`, scaling the duration with
    /// the great-circle arc length.
    pub fn to_target(
        camera: &Camera,
        target_lonlat: (f64, f64),
        target_zoom: f64,
        started_at: f64,
    ) -> FlyTo {
        let start_lonlat = camera.center_lonlat;
        let start_zoom = camera.zoom;
        let omega = great_circle_arc(start_lonlat, target_lonlat);
        let t = omega / std::f64::consts::PI;
        let duration = MIN_DURATION_S + (MAX_DURATION_S - MIN_DURATION_S) * t;
        FlyTo {
            start_lonlat,
            start_zoom,
            target_lonlat,
            target_zoom: target_zoom.clamp(camera::MIN_ZOOM, camera::MAX_ZOOM),
            started_at,
            duration,
        }
    }

    /// Parametric progress at `now` ∈ `[0, 1]`. Returns `1.0` past
    /// the end so callers can detect completion with `t >= 1.0`.
    pub fn parametric_t(&self, now: f64) -> f64 {
        if self.duration <= 0.0 {
            return 1.0;
        }
        ((now - self.started_at) / self.duration).clamp(0.0, 1.0)
    }

    /// Sample the camera state at time `now`. The renderer assigns
    /// the result directly into the camera's `center_lonlat` and
    /// `zoom` each frame.
    pub fn sample(&self, now: f64) -> ((f64, f64), f64) {
        let t = smoothstep(self.parametric_t(now));
        let lonlat = slerp_lonlat(self.start_lonlat, self.target_lonlat, t);
        let zoom = two_stage_zoom(self.start_zoom, self.target_zoom, t, self.omega());
        (lonlat, zoom)
    }

    /// The fly's great-circle arc length in radians. Cached on
    /// demand because both `sample` and external callers (the
    /// bbox-fit path) want it.
    pub fn omega(&self) -> f64 {
        great_circle_arc(self.start_lonlat, self.target_lonlat)
    }

    /// True when `now` is at or past the fly's end. Callers clear
    /// the renderer's `Option<FlyTo>` when this returns true.
    pub fn is_done(&self, now: f64) -> bool {
        self.parametric_t(now) >= 1.0
    }
}

/// Standard smoothstep `3t² − 2t³`. Eases both ends so the camera
/// doesn't jerk into or out of motion.
fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Great-circle arc length between two `(lon, lat)` points on the
/// unit sphere, in radians. Returns `0.0` for coincident inputs.
pub fn great_circle_arc(a: (f64, f64), b: (f64, f64)) -> f64 {
    let p0 = lonlat_to_sphere(a);
    let p1 = lonlat_to_sphere(b);
    let dot = (p0[0] * p1[0] + p0[1] * p1[1] + p0[2] * p1[2]).clamp(-1.0, 1.0);
    dot.acos()
}

/// Slerp on the unit sphere between `(lon, lat)` endpoints, then
/// re-derive `(lon, lat)` from the interpolated sphere point.
///
/// Falls back to start (within `1e-9` of identity) when the
/// endpoints are coincident — `sin(0)` would divide by zero.
fn slerp_lonlat(a: (f64, f64), b: (f64, f64), t: f64) -> (f64, f64) {
    let p0 = lonlat_to_sphere(a);
    let p1 = lonlat_to_sphere(b);
    let dot = (p0[0] * p1[0] + p0[1] * p1[1] + p0[2] * p1[2]).clamp(-1.0, 1.0);
    let omega = dot.acos();
    if omega.abs() < 1e-9 {
        return a;
    }
    let sin_omega = omega.sin();
    let w0 = ((1.0 - t) * omega).sin() / sin_omega;
    let w1 = (t * omega).sin() / sin_omega;
    let p = [
        w0 * p0[0] + w1 * p1[0],
        w0 * p0[1] + w1 * p1[1],
        w0 * p0[2] + w1 * p1[2],
    ];
    sphere_to_lonlat(p)
}

/// Two-stage zoom interpolation: zoom from start out to an
/// intermediate, then in to the target. The intermediate is
/// scaled by the arc length so antipodal flies pull all the way
/// back to globe view while short flies barely zoom out at all.
fn two_stage_zoom(start_zoom: f64, target_zoom: f64, t: f64, omega: f64) -> f64 {
    let dwell_below = start_zoom.min(target_zoom);
    // Antipodal: omega = π → intermediate = GLOBE_VIEW_ZOOM (1.5).
    // Coincident: omega = 0 → intermediate = dwell_below
    // (no zoom-out happens — already the right scale).
    let arc_t = (omega / std::f64::consts::PI).clamp(0.0, 1.0);
    let intermediate_zoom = dwell_below * (1.0 - arc_t) + GLOBE_VIEW_ZOOM * arc_t;
    // Stage A: [0, 0.5] → start → intermediate.
    // Stage B: [0.5, 1.0] → intermediate → target.
    if t < 0.5 {
        let s = t * 2.0;
        start_zoom * (1.0 - s) + intermediate_zoom * s
    } else {
        let s = (t - 0.5) * 2.0;
        intermediate_zoom * (1.0 - s) + target_zoom * s
    }
}

/// `(lon, lat)` → unit-sphere `(x, y, z)`. **Convention pinned by
/// the memory `feedback_sphere_convention`:** prime meridian at
/// `+Z`, north pole at `+Y`. Same as the renderer's WGSL shader.
fn lonlat_to_sphere(lonlat: (f64, f64)) -> [f64; 3] {
    let lon = lonlat.0.to_radians();
    let lat = lonlat.1.to_radians();
    [lat.cos() * lon.sin(), lat.sin(), lat.cos() * lon.cos()]
}

/// Unit-sphere `(x, y, z)` → `(lon, lat)` in degrees. Inverse of
/// `lonlat_to_sphere`.
fn sphere_to_lonlat(p: [f64; 3]) -> (f64, f64) {
    let lat_rad = p[1].clamp(-1.0, 1.0).asin();
    let lon_rad = p[0].atan2(p[2]);
    (lon_rad.to_degrees(), lat_rad.to_degrees())
}

/// Solve for the flat-Mercator zoom that frames `bbox` (as
/// `[lon_min, lat_min, lon_max, lat_max]`) in `canvas` pixels with
/// a 10% margin on every side.
///
/// The slippy convention: `pixels_per_world = TILE_PIXELS * 2^z`,
/// and one world-unit is the full Mercator x-extent (or y-extent
/// in the projection). We want `pixels_per_world * world_extent =
/// canvas_size_with_margin`, so
/// `z = log2(canvas / (TILE_PIXELS * world_extent * margin))`.
///
/// Returns the smaller-fitting zoom of the two axes so the bbox
/// fits in both dimensions.
pub fn zoom_to_fit_bbox(bbox: [f64; 4], canvas: (u32, u32)) -> f64 {
    const MARGIN: f64 = 1.10;
    let (lon_min, lat_min, lon_max, lat_max) = (bbox[0], bbox[1], bbox[2], bbox[3]);
    let (w_min_x, w_min_y) = crs::lonlat_to_world(lon_min, lat_min);
    let (w_max_x, w_max_y) = crs::lonlat_to_world(lon_max, lat_max);
    let world_w = (w_max_x - w_min_x).abs().max(1e-12);
    // Mercator y increases southward — lat_max (north) → smaller
    // world_y, lat_min (south) → larger. `.abs()` makes the order
    // irrelevant for the extent.
    let world_h = (w_max_y - w_min_y).abs().max(1e-12);
    let canvas_w = canvas.0 as f64;
    let canvas_h = canvas.1 as f64;
    let z_w = (canvas_w / (camera::TILE_PIXELS * world_w * MARGIN)).log2();
    let z_h = (canvas_h / (camera::TILE_PIXELS * world_h * MARGIN)).log2();
    z_w.min(z_h).clamp(camera::MIN_ZOOM, camera::MAX_ZOOM)
}

/// Centre point (`(lon, lat)`) of a bounding box. The bbox-fit
/// fly-to targets this point at the `zoom_to_fit_bbox` zoom.
pub fn bbox_center(bbox: [f64; 4]) -> (f64, f64) {
    ((bbox[0] + bbox[2]) * 0.5, (bbox[1] + bbox[3]) * 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    fn sphere_close(a: [f64; 3], b: [f64; 3], eps: f64) -> bool {
        close(a[0], b[0], eps) && close(a[1], b[1], eps) && close(a[2], b[2], eps)
    }

    fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }

    fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    fn length(a: [f64; 3]) -> f64 {
        dot(a, a).sqrt()
    }

    fn normalize(a: [f64; 3]) -> [f64; 3] {
        let l = length(a).max(1e-30);
        [a[0] / l, a[1] / l, a[2] / l]
    }

    // ---------------- Slerp ----------------

    #[test]
    fn slerp_at_t0_returns_start() {
        let start = (-87.6, 41.9); // Chicago
        let target = (151.2, -33.9); // Sydney
        let (lon, lat) = slerp_lonlat(start, target, 0.0);
        assert!(close(lon, start.0, 1e-9));
        assert!(close(lat, start.1, 1e-9));
    }

    #[test]
    fn slerp_at_t1_returns_target() {
        let start = (-87.6, 41.9);
        let target = (151.2, -33.9);
        let (lon, lat) = slerp_lonlat(start, target, 1.0);
        assert!(close(lon, target.0, 1e-9));
        assert!(close(lat, target.1, 1e-9));
    }

    #[test]
    fn slerp_midpoint_is_on_great_circle_plane() {
        // The great-circle path lies in the plane through the
        // sphere centre and the two endpoints. Its normal is
        // p0 × p1. A point on the path satisfies p_mid · normal = 0.
        // Linear lon/lat lerp would fail this test.
        let start = (-21.94, 64.13); // Reykjavík
        let target = (151.21, -33.87); // Sydney
        let (lon, lat) = slerp_lonlat(start, target, 0.5);
        let p_mid = lonlat_to_sphere((lon, lat));
        let p0 = lonlat_to_sphere(start);
        let p1 = lonlat_to_sphere(target);
        let normal = normalize(cross(p0, p1));
        assert!(
            dot(p_mid, normal).abs() < 1e-9,
            "midpoint off the great-circle plane: dot = {}",
            dot(p_mid, normal)
        );
        // Also confirm it's on the unit sphere.
        assert!(close(length(p_mid), 1.0, 1e-9));
    }

    #[test]
    fn slerp_midpoint_bows_northward_vs_rhumb_for_reykjavik_sydney() {
        // The actual great-circle path from Reykjavík (64°N) to
        // Sydney (-34°S) goes OVER ASIA — across Siberia, not
        // south. So the slerp midpoint sits well north of the
        // rhumb-line midpoint, which is roughly the lat average
        // (15°N). This test pins the bow magnitude — if a future
        // refactor accidentally reverts to lon/lat lerp, the
        // midpoint lat would drop ~25° and this fails.
        let start = (-21.94, 64.13);
        let target = (151.21, -33.87);
        let rhumb_mid_lat = (start.1 + target.1) * 0.5; // ~15°
        let (_lon_mid, lat_mid) = slerp_lonlat(start, target, 0.5);
        assert!(
            lat_mid > rhumb_mid_lat + 10.0,
            "slerp midpoint should bow northward of the rhumb-line midpoint \
             ({lat_mid:.1}° vs rhumb {rhumb_mid_lat:.1}°)"
        );
    }

    #[test]
    fn slerp_coincident_endpoints_returns_start_without_nan() {
        let p = (-87.6, 41.9);
        let (lon, lat) = slerp_lonlat(p, p, 0.5);
        assert!(close(lon, p.0, 1e-9));
        assert!(close(lat, p.1, 1e-9));
    }

    // ---------------- Sphere convention round-trip ----------------

    #[test]
    fn lonlat_sphere_round_trip_pinned_to_renderer_convention() {
        // The convention is: prime meridian at +Z, north pole at +Y.
        // Pin specific values so any drift gets caught.
        let cases = [
            ((0.0, 0.0), [0.0, 0.0, 1.0]),    // prime meridian at +Z
            ((0.0, 90.0), [0.0, 1.0, 0.0]),   // north pole at +Y
            ((90.0, 0.0), [1.0, 0.0, 0.0]),   // +90° lon at +X
            ((-90.0, 0.0), [-1.0, 0.0, 0.0]), // -90° lon at -X
        ];
        for ((lon, lat), expected) in cases {
            let p = lonlat_to_sphere((lon, lat));
            assert!(
                sphere_close(p, expected, 1e-9),
                "({lon}, {lat}) → {p:?}, expected {expected:?}"
            );
            let (lon_back, lat_back) = sphere_to_lonlat(p);
            assert!(close(lon_back, lon, 1e-9));
            assert!(close(lat_back, lat, 1e-9));
        }
    }

    // ---------------- FlyTo ----------------

    #[test]
    fn flyto_duration_scales_with_distance() {
        let cam_local = Camera::new(0.0, 0.0, 10.0);
        let close_fly = FlyTo::to_target(&cam_local, (0.001, 0.001), 10.0, 0.0);
        let far_fly = FlyTo::to_target(&cam_local, (180.0, 0.0), 10.0, 0.0);
        assert!(
            close_fly.duration < far_fly.duration,
            "close fly={}, far fly={}",
            close_fly.duration,
            far_fly.duration
        );
        assert!(close_fly.duration >= MIN_DURATION_S);
        assert!(far_fly.duration <= MAX_DURATION_S + 1e-9);
    }

    #[test]
    fn flyto_sample_at_start_matches_start() {
        let cam_local = Camera::new(-87.6, 41.9, 12.0);
        let fly = FlyTo::to_target(&cam_local, (151.2, -33.9), 12.0, 0.0);
        let ((lon, lat), zoom) = fly.sample(0.0);
        assert!(close(lon, -87.6, 1e-9));
        assert!(close(lat, 41.9, 1e-9));
        assert!(close(zoom, 12.0, 1e-9));
    }

    #[test]
    fn flyto_sample_at_end_matches_target() {
        let cam_local = Camera::new(-87.6, 41.9, 12.0);
        let fly = FlyTo::to_target(&cam_local, (151.2, -33.9), 4.0, 0.0);
        let ((lon, lat), zoom) = fly.sample(fly.duration);
        assert!(close(lon, 151.2, 1e-6));
        assert!(close(lat, -33.9, 1e-6));
        assert!(close(zoom, 4.0, 1e-9));
    }

    #[test]
    fn flyto_long_fly_dips_below_start_and_target_zoom() {
        // Antipodal-ish fly should pull the intermediate zoom
        // down to around GLOBE_VIEW_ZOOM. Sample at t = 0.5 (the
        // zoom-out apex).
        let cam_local = Camera::new(0.0, 0.0, 12.0);
        let fly = FlyTo::to_target(&cam_local, (180.0, 0.0), 12.0, 0.0);
        let ((_lon, _lat), zoom_mid) = fly.sample(fly.duration * 0.5);
        assert!(
            zoom_mid < 12.0,
            "antipodal fly midpoint should zoom out, got {zoom_mid}"
        );
        assert!(
            zoom_mid <= GLOBE_VIEW_ZOOM + 0.1,
            "antipodal fly should reach near globe view, got {zoom_mid}"
        );
    }

    #[test]
    fn flyto_is_done_after_duration() {
        let cam_local = Camera::new(0.0, 0.0, 10.0);
        let fly = FlyTo::to_target(&cam_local, (1.0, 1.0), 10.0, 0.0);
        assert!(!fly.is_done(fly.duration * 0.5));
        assert!(fly.is_done(fly.duration));
        assert!(fly.is_done(fly.duration + 1.0));
    }

    // ---------------- Bbox fit ----------------

    #[test]
    fn zoom_to_fit_bbox_picks_smaller_fitting_zoom() {
        // A small bbox around Chicago — about 0.4° square.
        let bbox = [-87.85, 41.65, -87.45, 42.05];
        let canvas = (1600, 1200);
        let z = zoom_to_fit_bbox(bbox, canvas);
        // 0.4° of latitude at z=N covers TILE_PIXELS * 2^z *
        // (0.4° / 360°) pixels. For 1200 px and 0.4° we want
        // z such that 256 * 2^z * (0.4/360) ≈ 1200 — i.e. z ≈ 10.
        // Allow a wide band; the test is "ballpark," not exact.
        assert!(
            (7.0..=12.0).contains(&z),
            "Chicago bbox should land near z=10, got {z}"
        );
    }

    #[test]
    fn zoom_to_fit_bbox_clamps_to_max_zoom() {
        // Trivially small bbox — would compute a huge zoom.
        // Must clamp to MAX_ZOOM.
        let bbox = [0.0, 0.0, 1e-9, 1e-9];
        let z = zoom_to_fit_bbox(bbox, (800, 600));
        assert!(close(z, camera::MAX_ZOOM, 1e-9));
    }

    #[test]
    fn zoom_to_fit_bbox_full_world_lands_near_globe_view() {
        // A whole-world bbox at an 800×600 canvas gives roughly
        // z=log2(600/(256·1·1.1)) ≈ 1.0. That sits inside the
        // globe-view band (z < GLOBE_FLAT_ZOOM = 5.0), so the
        // result is a usable "everything visible" view rather
        // than a clamp.
        let bbox = [-180.0, -85.0, 180.0, 85.0];
        let z = zoom_to_fit_bbox(bbox, (800, 600));
        assert!(
            (camera::MIN_ZOOM..camera::GLOBE_FLAT_ZOOM).contains(&z),
            "full-world bbox should land in globe view, got z={z}"
        );
    }
}
