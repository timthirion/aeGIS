//! Sun direction at a given UNIX UTC instant, expressed as a unit
//! vector in the renderer's body-fixed frame (prime meridian at +Z,
//! north pole at +Y). Used by the day/night dim pass (plan 0009 M0)
//! and the dawn/dusk warm tint (M1).
//!
//! The math is a standard low-precision solar position algorithm
//! (Astronomical Almanac, low-precision formulas, valid for 1950–
//! 2050 with sub-arcminute error). Mean longitude + first-order
//! equation-of-center → ecliptic longitude → declination + right
//! ascension; subtract GMST to land in the Earth-fixed frame; then
//! spherical-to-Cartesian using the same prime-meridian-at-+Z
//! convention as `lonlat_to_sphere` in tile.wgsl / earth.wgsl /
//! caps.wgsl / vector.wgsl.
//!
//! When plan 0010 (time slider) ships, the slider drives the input
//! `unix_s` here so the user can scrub the terminator across the
//! globe; until then, the renderer feeds `SimClock::sim_unix_s` and
//! the terminator drifts with real time.

/// UNIX seconds at J2000.0 (2000-01-01 12:00:00 UTC).
const J2000_UNIX: f64 = 946_728_000.0;

/// Sub-solar geographic point in **radians** at the given instant.
/// Returns `(lon, lat)` with `lon ∈ (−π, π]` and
/// `lat ∈ [−ε, +ε]` (where ε ≈ 23.44° is the obliquity).
pub fn subsolar_radians(unix_s: f64) -> (f64, f64) {
    let n = (unix_s - J2000_UNIX) / 86_400.0;
    let mean_lon_deg = (280.460 + 0.985_647_4 * n).rem_euclid(360.0);
    let mean_anom_rad = (357.528 + 0.985_600_3 * n).rem_euclid(360.0).to_radians();
    let ecl_lon_deg =
        mean_lon_deg + 1.915 * mean_anom_rad.sin() + 0.020 * (2.0 * mean_anom_rad).sin();
    let ecl_lon_rad = ecl_lon_deg.to_radians();
    let obliquity_rad = (23.439 - 0.000_000_4 * n).to_radians();
    let sin_ecl = ecl_lon_rad.sin();
    let ra = (sin_ecl * obliquity_rad.cos()).atan2(ecl_lon_rad.cos());
    let dec = (sin_ecl * obliquity_rad.sin()).asin();
    let gmst_rad = (280.460_618_37 + 360.985_647_366_29 * n)
        .rem_euclid(360.0)
        .to_radians();
    let two_pi = 2.0 * std::f64::consts::PI;
    let mut lon = (ra - gmst_rad).rem_euclid(two_pi);
    if lon > std::f64::consts::PI {
        lon -= two_pi;
    }
    (lon, dec)
}

/// Sun direction unit vector in the renderer's body-fixed frame
/// (prime meridian at +Z, north pole at +Y). Matches the shaders'
/// `lonlat_to_sphere` convention.
pub fn direction_from_unix(unix_s: f64) -> [f32; 3] {
    let (lon, lat) = subsolar_radians(unix_s);
    let cos_lat = lat.cos();
    [
        (cos_lat * lon.sin()) as f32,
        lat.sin() as f32,
        (cos_lat * lon.cos()) as f32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_is_unit_length_across_a_year() {
        // Sweep a full year in 1-day steps; magnitude must stay 1.
        for i in 0..366 {
            let t = J2000_UNIX + (i as f64) * 86_400.0;
            let d = direction_from_unix(t);
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-5, "len = {len} at +{i}d");
        }
    }

    #[test]
    fn subsolar_lat_stays_within_obliquity() {
        // The sub-solar latitude is the solar declination, which
        // never exceeds ±23.44° (the obliquity of the ecliptic).
        // If it does, the math is wrong somewhere.
        for i in 0..366 {
            let t = J2000_UNIX + (i as f64) * 86_400.0;
            let (_, lat) = subsolar_radians(t);
            assert!(lat.to_degrees().abs() < 23.5, "lat = {}°", lat.to_degrees());
        }
    }

    #[test]
    fn earth_rotation_in_12_hours_is_about_180_degrees() {
        // The sub-solar point sweeps westward at ~15°/hour. Twelve
        // hours later, the sun is on the opposite side of Earth —
        // direction should be roughly anti-parallel. Use the dot
        // product as the test (must be < -0.9 for "near-opposite").
        let now = 1_773_043_200.0; // 2026-03-21 12:00 UTC
        let later = now + 12.0 * 3600.0;
        let a = direction_from_unix(now);
        let b = direction_from_unix(later);
        let dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        assert!(dot < -0.9, "dot = {dot} — sun didn't rotate ~180°");
    }

    #[test]
    fn subsolar_lat_crosses_zero_twice_per_year() {
        // The sub-solar latitude goes negative in northern winter,
        // crosses zero at the March equinox, positive in summer,
        // back to zero at the September equinox — so there are
        // exactly two sign-change crossings in any contiguous
        // 365-day window. A formula with a sign or constant error
        // either flattens the curve or shifts it off the equator,
        // both of which break this invariant.
        let start = 1_767_225_600.0; // 2026-01-01 00:00 UTC
        let mut crossings = 0;
        let (_, mut prev_lat) = subsolar_radians(start);
        for i in 1..366 {
            let t = start + (i as f64) * 86_400.0;
            let (_, lat) = subsolar_radians(t);
            if prev_lat.signum() != lat.signum() {
                crossings += 1;
            }
            prev_lat = lat;
        }
        assert_eq!(
            crossings, 2,
            "expected 2 equinox crossings, got {crossings}"
        );
    }
}
