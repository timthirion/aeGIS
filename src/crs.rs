//! Coordinate reference system primitives.
//!
//! M1 ships **Spherical Mercator** (the EPSG:3857 web-mapping convention)
//! — forward, inverse, and the tile-coordinate flavour that drops out of
//! it. WGS84 ellipsoidal projections + the long tail of EPSG codes arrive
//! in Phase 3 via `proj4rs` (a separate plan).
//!
//! Convention: every public function takes `lon, lat` (in that order, in
//! degrees) and returns `(x, y)` (in that order). Mixing the two is the
//! single most common GIS bug class; the public surface here is named so
//! that swapping arguments produces a different compile error or a
//! visibly wrong result, not a silent fifty-mile offset. See
//! `crs-skeptic`'s mandate.

use std::f64::consts::PI;

/// The pole-side cutoff Spherical Mercator is defined within. Beyond
/// `|lat| > MERCATOR_LAT_MAX` the projection diverges — Y goes to ∞ at
/// ±90°. The standard web-map convention is to clamp here.
pub const MERCATOR_LAT_MAX_DEG: f64 = 85.05112877980659;

/// Forward Spherical Mercator: WGS84 `lon` / `lat` (degrees) →
/// **normalised Mercator** in `[0, 1] × [0, 1]`, where `(0, 0)` is the
/// north-west corner of the world (`-180°` lon, `+MERCATOR_LAT_MAX` lat)
/// and `(1, 1)` is the south-east corner.
///
/// Latitude is clamped to `±MERCATOR_LAT_MAX_DEG` so the result is
/// always finite. Longitude wraps modulo 360°; out-of-range inputs are
/// treated literally (no auto-wrap to `[-180, 180]`).
pub fn lonlat_to_world(lon: f64, lat: f64) -> (f64, f64) {
    let lat = lat.clamp(-MERCATOR_LAT_MAX_DEG, MERCATOR_LAT_MAX_DEG);
    let x = (lon + 180.0) / 360.0;
    let lat_rad = lat.to_radians();
    // y in [0, 1]; the `asinh(tan)` form is equivalent to the more
    // common `ln(tan + sec)` but doesn't overflow as catastrophically
    // near the clamped poles.
    let y = (1.0 - lat_rad.tan().asinh() / PI) / 2.0;
    (x, y)
}

/// Inverse Spherical Mercator: normalised `(x, y)` → WGS84
/// `(lon, lat)` in degrees. Inverse of [`lonlat_to_world`] up to the
/// latitude clamp.
pub fn world_to_lonlat(x: f64, y: f64) -> (f64, f64) {
    let lon = x * 360.0 - 180.0;
    let n = PI * (1.0 - 2.0 * y);
    let lat = n.sinh().atan().to_degrees();
    (lon, lat)
}

/// Fractional XYZ tile coordinate for the given `(lon, lat)` at zoom
/// `z`. Returns `(x, y)` where `x ∈ [0, 2^z]` increases east and
/// `y ∈ [0, 2^z]` increases south (the OSM / XYZ convention, opposite
/// of TMS which is south-up). Floor each component for the integer
/// tile address.
pub fn lonlat_to_tile_fractional(z: u8, lon: f64, lat: f64) -> (f64, f64) {
    let (wx, wy) = lonlat_to_world(lon, lat);
    let n = (1u64 << z) as f64;
    (wx * n, wy * n)
}

/// The `(lon, lat)` of the **north-west corner** of the integer XYZ
/// tile `(z, x, y)`. Inverse of `floor(lonlat_to_tile_fractional)` —
/// up to the half-tile snap.
pub fn tile_to_lonlat_nw(z: u8, x: u32, y: u32) -> (f64, f64) {
    let n = (1u64 << z) as f64;
    world_to_lonlat(x as f64 / n, y as f64 / n)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // Forward / inverse round-trips
    // -----------------------------------------------------------------

    #[test]
    fn origin_maps_to_world_center() {
        let (x, y) = lonlat_to_world(0.0, 0.0);
        assert!((x - 0.5).abs() < 1e-12, "x = {x}");
        assert!((y - 0.5).abs() < 1e-12, "y = {y}");
    }

    #[test]
    fn world_center_maps_to_origin() {
        let (lon, lat) = world_to_lonlat(0.5, 0.5);
        assert!(lon.abs() < 1e-9, "lon = {lon}");
        assert!(lat.abs() < 1e-9, "lat = {lat}");
    }

    #[test]
    fn antimeridian_corners() {
        // NW corner of the world: (-180°, +MERCATOR_LAT_MAX) → (0, 0)
        let (x, y) = lonlat_to_world(-180.0, MERCATOR_LAT_MAX_DEG);
        assert!(x.abs() < 1e-12, "NW x = {x}");
        assert!(y.abs() < 1e-12, "NW y = {y}");

        // SE corner: (+180°, -MERCATOR_LAT_MAX) → (1, 1)
        let (x, y) = lonlat_to_world(180.0, -MERCATOR_LAT_MAX_DEG);
        assert!((x - 1.0).abs() < 1e-12, "SE x = {x}");
        assert!((y - 1.0).abs() < 1e-12, "SE y = {y}");
    }

    #[test]
    fn round_trip_grid_stable_to_1e_9() {
        // A grid over the projection's defined domain. Round-tripping a
        // (lon, lat) through forward+inverse must recover the input to
        // within the tolerance — anywhere this fails is a CRS-skeptic
        // P0 (silent coordinate drift).
        let tol = 1e-9;
        let mut max_dlon = 0.0_f64;
        let mut max_dlat = 0.0_f64;
        for lon_step in -18..=18 {
            for lat_step in -85..=85 {
                let lon = lon_step as f64 * 10.0;
                let lat = lat_step as f64;
                let (x, y) = lonlat_to_world(lon, lat);
                let (lon2, lat2) = world_to_lonlat(x, y);
                let dlon = (lon - lon2).abs();
                let dlat = (lat - lat2).abs();
                max_dlon = max_dlon.max(dlon);
                max_dlat = max_dlat.max(dlat);
                assert!(
                    dlon < tol,
                    "lon round-trip at ({lon}, {lat}): {lon} ≠ {lon2}"
                );
                assert!(
                    dlat < tol,
                    "lat round-trip at ({lon}, {lat}): {lat} ≠ {lat2}"
                );
            }
        }
        // Sanity log — would surface as a regression if max errors grow.
        eprintln!("round-trip max Δlon = {max_dlon:e}, Δlat = {max_dlat:e}");
    }

    #[test]
    fn latitude_clamps_at_pole() {
        // Inputs beyond the projection's defined range must clamp
        // (rather than return ±∞ or NaN) — silent NaN propagation
        // through the tile-selector is the crs-skeptic-mandated
        // P0 we're defending against.
        let (_, y_clamped) = lonlat_to_world(0.0, 89.0);
        let (_, y_at_max) = lonlat_to_world(0.0, MERCATOR_LAT_MAX_DEG);
        assert!(y_clamped.is_finite());
        assert!((y_clamped - y_at_max).abs() < 1e-12);

        let (_, y_clamped_s) = lonlat_to_world(0.0, -89.0);
        let (_, y_at_min) = lonlat_to_world(0.0, -MERCATOR_LAT_MAX_DEG);
        assert!(y_clamped_s.is_finite());
        assert!((y_clamped_s - y_at_min).abs() < 1e-12);
    }

    // -----------------------------------------------------------------
    // Tile math
    // -----------------------------------------------------------------

    #[test]
    fn z0_has_one_tile() {
        // At zoom 0, the entire world fits in tile (0, 0).
        let (fx, fy) = lonlat_to_tile_fractional(0, 0.0, 0.0);
        assert!((fx - 0.5).abs() < 1e-12, "fx = {fx}");
        assert!((fy - 0.5).abs() < 1e-12, "fy = {fy}");
    }

    #[test]
    fn z1_quadrants() {
        // At z=1 the world splits into 2×2 tiles. The four quadrants
        // around the origin pin which way the integer coordinate falls
        // — the silent-killer lon/lat-swap bug fails this test.
        let cases = [
            //  lon,    lat, expected (floor(fx), floor(fy))
            ((0.001, 0.001), (1, 0)),   // east + north
            ((-0.001, 0.001), (0, 0)),  // west + north
            ((-0.001, -0.001), (0, 1)), // west + south
            ((0.001, -0.001), (1, 1)),  // east + south
        ];
        for ((lon, lat), expected) in cases {
            let (fx, fy) = lonlat_to_tile_fractional(1, lon, lat);
            let got = (fx.floor() as u32, fy.floor() as u32);
            assert_eq!(got, expected, "tile at ({lon}, {lat})");
        }
    }

    #[test]
    fn chicago_tile_z10_matches_osm_fixture() {
        // Chicago: 41.8781° N, 87.6298° W.
        // Verified against https://tile.openstreetmap.org/10/262/380.png
        // — the actual OSM tile that's served at this address shows the
        // Chicago metro area with Lake Michigan to the right.
        let (fx, fy) = lonlat_to_tile_fractional(10, -87.6298, 41.8781);
        assert_eq!(fx.floor() as u32, 262, "Chicago x at z=10 (fx = {fx})");
        assert_eq!(fy.floor() as u32, 380, "Chicago y at z=10 (fy = {fy})");
    }

    #[test]
    fn tile_nw_corner_round_trip() {
        // The NW corner of tile (z, x, y) projects forward to exactly
        // that (z, fx=x, fy=y).
        for &(z, x, y) in &[(0, 0, 0), (1, 0, 0), (1, 1, 1), (10, 262, 380)] {
            let (lon, lat) = tile_to_lonlat_nw(z, x, y);
            let (fx, fy) = lonlat_to_tile_fractional(z, lon, lat);
            assert!(
                (fx - x as f64).abs() < 1e-9,
                "tile NW round-trip at ({z}, {x}, {y}): fx = {fx}"
            );
            assert!(
                (fy - y as f64).abs() < 1e-9,
                "tile NW round-trip at ({z}, {x}, {y}): fy = {fy}"
            );
        }
    }
}
