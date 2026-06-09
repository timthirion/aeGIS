//! Satellite orbits — Phase 10's first cut.
//!
//! Each `Orbit` is a Keplerian description (semi-major axis,
//! eccentricity, inclination, RAAN, argument of periapsis) of a real
//! satellite or constellation. We sample the orbit around one full
//! revolution and hand the renderer a closed loop of line segments
//! around the globe.
//!
//! Why pure Kepler rather than SGP4 + TLEs?
//! - **No external dependencies and no staleness.** SGP4 needs a
//!   current TLE to give meaningful positions; bundled TLEs go stale
//!   in days. The orbit *shape* is what we render, and that's stable
//!   from the elements alone.
//! - **No `chrono` on wasm.** SGP4 propagation needs a UTC datetime;
//!   the popular Rust SGP4 crate pulls chrono in by default.
//! - **Visual goal is orbital wireframes.** We're not predicting where
//!   the ISS is right now — we're showing the *shape* of its orbit
//!   around Earth. Kepler is sufficient and clearer.
//!
//! Real per-satellite propagation (current position, ground track,
//! pass prediction) lands in a follow-on commit on top of these
//! same line buffers.

use std::f64::consts::PI;

/// Earth's mean equatorial radius, in km. The sphere we render has
/// radius 1; satellite distances divide by this constant to express
/// the orbit in sphere units.
const EARTH_RADIUS_KM: f64 = 6378.137;

/// A bundled orbit — Keplerian elements plus a label and the RGB
/// colour to render its track in. RAAN and argument-of-periapsis
/// values are illustrative (they only rotate the orbit around Earth);
/// inclination, semi-major axis, and eccentricity are real.
#[derive(Debug, Clone, Copy)]
pub struct Orbit {
    pub name: &'static str,
    /// Semi-major axis in km. For circular LEO, ≈ `6378 + altitude`.
    pub semi_major_axis_km: f64,
    /// 0 = circle, → 1 = parabola. Real values for LEO are ~1e-4;
    /// Molniya/Tundra orbits run ~0.7.
    pub eccentricity: f64,
    /// Inclination of the orbital plane relative to the equator, in
    /// degrees. 0 = equatorial, 90 = polar.
    pub inclination_deg: f64,
    /// Right ascension of the ascending node (Ω), in degrees.
    pub raan_deg: f64,
    /// Argument of periapsis (ω), in degrees. For circular orbits
    /// it's degenerate but doesn't affect the path shape.
    pub arg_periapsis_deg: f64,
    /// RGB line colour in linear-light space (sRGB target applies the
    /// gamma transform on output).
    pub color: [f32; 3],
}

/// Bundled satellite catalog — eight orbits chosen to span the
/// interesting altitude / inclination space:
/// LEO equatorial-ish, LEO polar, MEO (GNSS), GEO, HEO (Molniya/Tundra).
pub const SATELLITES: &[Orbit] = &[
    // International Space Station — 408 km, 51.6° inclination.
    Orbit {
        name: "ISS",
        semi_major_axis_km: EARTH_RADIUS_KM + 408.0,
        eccentricity: 0.0006,
        inclination_deg: 51.64,
        raan_deg: 0.0,
        arg_periapsis_deg: 0.0,
        color: [0.40, 0.90, 1.00],
    },
    // Hubble Space Telescope — 540 km, 28.5° inclination.
    Orbit {
        name: "HST",
        semi_major_axis_km: EARTH_RADIUS_KM + 540.0,
        eccentricity: 0.0003,
        inclination_deg: 28.47,
        raan_deg: 60.0,
        arg_periapsis_deg: 0.0,
        color: [1.00, 0.70, 0.30],
    },
    // Starlink representative — 550 km, 53° (one shell of many).
    Orbit {
        name: "Starlink",
        semi_major_axis_km: EARTH_RADIUS_KM + 550.0,
        eccentricity: 0.0001,
        inclination_deg: 53.0,
        raan_deg: 30.0,
        arg_periapsis_deg: 0.0,
        color: [0.85, 0.85, 0.95],
    },
    // Sentinel-1 — 693 km, near-polar sun-synchronous (98.18°).
    Orbit {
        name: "Sentinel-1",
        semi_major_axis_km: EARTH_RADIUS_KM + 693.0,
        eccentricity: 0.0001,
        inclination_deg: 98.18,
        raan_deg: 90.0,
        arg_periapsis_deg: 0.0,
        color: [0.40, 0.95, 0.50],
    },
    // Iridium NEXT — 780 km, near-polar (86.4°).
    Orbit {
        name: "Iridium",
        semi_major_axis_km: EARTH_RADIUS_KM + 780.0,
        eccentricity: 0.0002,
        inclination_deg: 86.40,
        raan_deg: 130.0,
        arg_periapsis_deg: 0.0,
        color: [1.00, 0.55, 0.85],
    },
    // GPS BIIR — semi-major axis ~26,560 km, 55° inclination (MEO).
    Orbit {
        name: "GPS",
        semi_major_axis_km: 26560.0,
        eccentricity: 0.005,
        inclination_deg: 55.0,
        raan_deg: 0.0,
        arg_periapsis_deg: 0.0,
        color: [1.00, 0.95, 0.40],
    },
    // GOES-East geostationary — 42,164 km, 0° (equatorial).
    Orbit {
        name: "GOES-East",
        semi_major_axis_km: 42164.0,
        eccentricity: 0.0001,
        inclination_deg: 0.05,
        raan_deg: 0.0,
        arg_periapsis_deg: 0.0,
        color: [1.00, 0.40, 0.30],
    },
    // Tundra — highly elliptical, 24h period, 63.4° (Molniya-family).
    Orbit {
        name: "Tundra",
        semi_major_axis_km: 42164.0,
        eccentricity: 0.27,
        inclination_deg: 63.4,
        raan_deg: 280.0,
        arg_periapsis_deg: 270.0,
        color: [0.80, 0.55, 1.00],
    },
];

/// One LineList vertex: position (sphere-radius units) + colour.
/// Layout matches the WGSL `VsIn` in `satellites.wgsl`.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SatVertex {
    pub pos: [f32; 3],
    pub color: [f32; 3],
}

/// Build the renderer's vertex buffer for every orbit in `SATELLITES`.
/// Each orbit is sampled at `samples` true-anomaly steps and emitted
/// as `samples` LineList segments (`2 * samples` vertices) — i.e. a
/// closed ring per satellite, all in one buffer for a single draw.
pub fn build_orbit_vertices(samples: usize) -> Vec<SatVertex> {
    let mut out = Vec::with_capacity(SATELLITES.len() * samples * 2);
    for orbit in SATELLITES {
        let path = orbital_path(orbit, samples);
        // path has `samples + 1` points, the last duplicating the
        // first to close the loop. Emit consecutive pairs.
        for i in 0..samples {
            out.push(SatVertex {
                pos: path[i],
                color: orbit.color,
            });
            out.push(SatVertex {
                pos: path[i + 1],
                color: orbit.color,
            });
        }
    }
    out
}

/// Sample one full revolution of an orbit at `samples + 1` true-anomaly
/// values (the last duplicates the first to close the ring). Positions
/// are returned in sphere-radius units, in the *render* coordinate
/// frame: +Y = north pole, +Z = prime meridian, +X = 90°E. This matches
/// the convention `lonlat_to_sphere` uses everywhere else in the crate
/// — see `feedback-sphere-convention` for the why.
fn orbital_path(orbit: &Orbit, samples: usize) -> Vec<[f32; 3]> {
    let a = orbit.semi_major_axis_km;
    let e = orbit.eccentricity;
    let p = a * (1.0 - e * e);
    let i_rad = orbit.inclination_deg.to_radians();
    let raan_rad = orbit.raan_deg.to_radians();
    let argp_rad = orbit.arg_periapsis_deg.to_radians();
    let scale = 1.0 / EARTH_RADIUS_KM;
    let (ci, si) = (i_rad.cos(), i_rad.sin());
    let (co, so) = (raan_rad.cos(), raan_rad.sin());
    let (cw, sw) = (argp_rad.cos(), argp_rad.sin());

    let mut out = Vec::with_capacity(samples + 1);
    for k in 0..=samples {
        let nu = 2.0 * PI * (k as f64) / (samples as f64);
        let r = p / (1.0 + e * nu.cos());
        // Perifocal frame: x toward periapsis, y 90° later in the
        // direction of motion, z normal to the orbit plane.
        let x_p = r * nu.cos();
        let y_p = r * nu.sin();

        // Rotate perifocal → ECI: R3(Ω) · R1(i) · R3(ω) · perifocal.
        // After R3(ω) (rotate around Z by ω):
        let x1 = cw * x_p - sw * y_p;
        let y1 = sw * x_p + cw * y_p;
        let z1 = 0.0;
        // After R1(i) (rotate around X by i):
        let x2 = x1;
        let y2 = ci * y1 - si * z1;
        let z2 = si * y1 + ci * z1;
        // After R3(Ω) (rotate around Z by Ω):
        let x_eci = co * x2 - so * y2;
        let y_eci = so * x2 + co * y2;
        let z_eci = z2;

        // Render-frame remap. The crate's sphere convention puts the
        // prime meridian on +Z and the north pole on +Y; ECI uses +Z
        // for the north pole. So sphere_X ← ECI_y, sphere_Y ← ECI_z,
        // sphere_Z ← ECI_x.
        out.push([
            (y_eci * scale) as f32,
            (z_eci * scale) as f32,
            (x_eci * scale) as f32,
        ]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_nonempty_and_named() {
        assert!(
            !SATELLITES.is_empty(),
            "satellite catalog must not be empty"
        );
        for orbit in SATELLITES {
            assert!(
                !orbit.name.is_empty(),
                "orbit at semi-major-axis {} km has empty name",
                orbit.semi_major_axis_km
            );
            assert!(
                orbit.semi_major_axis_km > EARTH_RADIUS_KM,
                "{}: semi-major-axis {} ≤ Earth radius",
                orbit.name,
                orbit.semi_major_axis_km
            );
            assert!(
                (0.0..1.0).contains(&orbit.eccentricity),
                "{}: eccentricity {} out of [0, 1)",
                orbit.name,
                orbit.eccentricity
            );
        }
    }

    #[test]
    fn orbital_path_closes_on_itself() {
        // The last sample must equal the first — the renderer relies
        // on this to close each ring without a special-case wrap.
        for orbit in SATELLITES {
            let path = orbital_path(orbit, 64);
            assert_eq!(path.len(), 65, "{}: path length", orbit.name);
            let d = (0..3)
                .map(|i| (path[0][i] - path[64][i]).powi(2))
                .sum::<f32>()
                .sqrt();
            assert!(
                d < 1e-5,
                "{}: ring doesn't close (first={:?}, last={:?}, dist={})",
                orbit.name,
                path[0],
                path[64],
                d
            );
        }
    }

    #[test]
    fn circular_orbit_radius_is_constant() {
        // A near-circular orbit (e ≈ 0) should produce path points all
        // at roughly the same distance from origin. Tolerance allows
        // for the bundled non-zero eccentricities.
        let iss = &SATELLITES[0];
        assert!(iss.eccentricity < 0.01, "ISS test expects circular orbit");
        let path = orbital_path(iss, 128);
        let expected_r = (iss.semi_major_axis_km / EARTH_RADIUS_KM) as f32;
        for (i, p) in path.iter().enumerate() {
            let r = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
            let drift = (r - expected_r).abs() / expected_r;
            assert!(
                drift < 0.01,
                "ISS sample {} radius {} vs expected {} (drift {})",
                i,
                r,
                expected_r,
                drift
            );
        }
    }

    #[test]
    fn polar_orbit_passes_over_poles() {
        // A 90°-ish inclination orbit must cross the north pole (have
        // a sample with high |y|). Sentinel-1 is 98° inclination.
        let sentinel = SATELLITES
            .iter()
            .find(|o| o.name == "Sentinel-1")
            .expect("Sentinel-1 in catalog");
        let path = orbital_path(sentinel, 256);
        let max_abs_y = path.iter().map(|p| p[1].abs()).fold(0.0_f32, f32::max);
        let expected_r = (sentinel.semi_major_axis_km / EARTH_RADIUS_KM) as f32;
        // For inclination 98°, the orbit's max |y| is sin(98°) * r ≈
        // 0.99 * r. Allow 5% slack for sampling alignment.
        assert!(
            max_abs_y > 0.94 * expected_r,
            "Sentinel-1 max |y| = {} < expected ≈ {}",
            max_abs_y,
            0.99 * expected_r
        );
    }

    #[test]
    fn equatorial_orbit_stays_in_equatorial_plane() {
        // GOES is ~0° inclination; |y| should be near zero everywhere.
        let goes = SATELLITES
            .iter()
            .find(|o| o.name == "GOES-East")
            .expect("GOES-East in catalog");
        let path = orbital_path(goes, 64);
        let max_abs_y = path.iter().map(|p| p[1].abs()).fold(0.0_f32, f32::max);
        let expected_r = (goes.semi_major_axis_km / EARTH_RADIUS_KM) as f32;
        // 0.05° inclination → max |y| ≈ sin(0.05°) · r ≈ 0.001 · r.
        // Give ample slack but catch any axis-swap regression.
        assert!(
            max_abs_y < 0.02 * expected_r,
            "GOES (≈equatorial) max |y| = {} too large vs expected_r = {}",
            max_abs_y,
            expected_r
        );
    }

    #[test]
    fn build_orbit_vertices_emits_a_line_segment_per_sample() {
        let samples = 32;
        let verts = build_orbit_vertices(samples);
        // 2 verts per segment × samples segments × #orbits.
        assert_eq!(verts.len(), 2 * samples * SATELLITES.len());
        // Per-segment colour must match the source orbit's colour.
        for (orbit_idx, orbit) in SATELLITES.iter().enumerate() {
            let base = orbit_idx * samples * 2;
            assert_eq!(verts[base].color, orbit.color, "{} colour", orbit.name);
        }
    }
}
