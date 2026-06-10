//! Satellite orbit math — Two-Line Element (TLE) parsing,
//! SGP4 propagation, and TEME → ECEF rotation. Plan 0004 M0.
//!
//! Convention: this module returns **render-space** sphere
//! coordinates where 1.0 = one Earth equatorial radius
//! (6378.137 km). The renderer's body-fixed sphere has prime
//! meridian at `+Z`, north pole at `+Y` (per the
//! `feedback_sphere_convention` memory) — different from
//! standard ECEF (prime meridian at `+X`, north pole at `+Z`).
//! We swap axes at the boundary so call sites don't have to
//! think about it.
//!
//! Coordinate-frame approximation: we model TEME → ECEF as a
//! single Z-axis rotation by GMST. That's accurate to ~5 km at
//! orbital altitudes — fine for "watch the ISS cross the
//! Atlantic," wrong for sub-km pointing. Documented in plan 0004.

use std::f64::consts::PI;

use sgp4::{Constants, Elements};

/// Earth's equatorial radius in km. Render-space units divide by
/// this so 1.0 == one Earth radius.
pub const EARTH_RADIUS_KM: f64 = 6378.137;

/// Coarse classification for satellites — drives the point colour
/// and the toggleable category pills in the M2 UI.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Category {
    /// Crewed + uncrewed stations. ISS, Tiangong, etc.
    Stations,
    /// Starlink LEO constellation.
    Starlink,
    /// GPS / GLONASS / Galileo / BeiDou navigation constellations.
    Gnss,
    /// Weather + Earth-observation polar orbiters.
    Weather,
    /// Tracked debris.
    Debris,
    /// Anything that doesn't fit a tagged category.
    Other,
}

impl Category {
    /// RGB display colour for points in this category (sRGB8).
    pub fn color_srgb8(self) -> [u8; 3] {
        match self {
            Category::Stations => [255, 244, 100], // warm yellow
            Category::Starlink => [120, 200, 255], // pale blue
            Category::Gnss => [255, 160, 120],     // amber
            Category::Weather => [180, 240, 180],  // light green
            Category::Debris => [200, 200, 200],   // grey
            Category::Other => [220, 220, 220],
        }
    }
}

/// Raw TLE — the three lines as Celestrak serves them.
#[derive(Clone, Debug)]
pub struct Tle {
    pub name: String,
    pub line1: String,
    pub line2: String,
}

/// A TLE prepared for repeated propagation. Owns the SGP4 elements
/// + the constants the propagator pre-computes from them.
pub struct Satellite {
    pub name: String,
    pub norad_id: u32,
    pub category: Category,
    /// UNIX seconds (UTC) the TLE was measured at. Used to convert
    /// `sim_unix_s` to "minutes since TLE epoch" for `propagate`.
    pub epoch_unix_s: f64,
    elements: Elements,
    constants: Constants,
}

impl Satellite {
    /// Mean motion in revolutions / day, straight from the TLE.
    /// Used by the trail-points sampler to size one orbital period.
    pub fn mean_motion_revs_per_day(&self) -> f64 {
        // `sgp4::Elements::mean_motion` is already in revs/day
        // (matching the TLE field), not radians/minute.
        self.elements.mean_motion
    }

    /// One orbital period in minutes. For LEO satellites (ISS:
    /// ~93 min, Starlink: ~95 min) this is sensible; for
    /// geostationary (~1440 min) it's accurate but rarely useful.
    pub fn orbital_period_minutes(&self) -> f64 {
        let revs_per_day = self.mean_motion_revs_per_day();
        if revs_per_day.abs() < 1e-9 {
            // Defensive: a zero mean motion is malformed TLE data.
            1440.0
        } else {
            1440.0 / revs_per_day
        }
    }

    /// Sample positions along one orbital period centred on
    /// `sim_unix_s`. Returns `n` render-space points the trail
    /// shader can draw as a LineStrip. Points that fail SGP4
    /// propagation are skipped (the caller gets fewer than `n`
    /// vertices). Plan 0004 M3.
    pub fn trail_points(&self, sim_unix_s: f64, n: usize) -> Vec<[f32; 3]> {
        let period_s = self.orbital_period_minutes() * 60.0;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            // Sample from `t - period/2` to `t + period/2`, so the
            // trail wraps around to itself (LEO orbits repeat each
            // period, so the two endpoints land on the same lat/lon
            // band — the visual trail closes).
            let frac = i as f64 / (n - 1).max(1) as f64;
            let t = sim_unix_s + (frac - 0.5) * period_s;
            if let Some(pos) = propagate_render_space(self, t) {
                out.push(pos);
            }
        }
        out
    }
}

/// Parse Celestrak-format TLE text into raw `Tle` records.
/// Accepts both 3-line (name + 2 elements) and 2-line (no name)
/// blocks; missing names get a synthetic `"NORAD <id>"` label so
/// no satellite ever loses its identity.
pub fn parse_tles(text: &str) -> Vec<Tle> {
    // Celestrak's `gp.php` rate-limits per-IP by GROUP: a repeat
    // fetch within 2 hours gets a polite plain-text "GP data has
    // not updated since your last successful download" body
    // instead of TLE data. Detect this and return an empty Vec
    // so the renderer's `load_satellites` no-ops gracefully
    // (existing catalog stays intact; the toggle isn't broken,
    // just the fresh data didn't arrive). Plan 0004 M2 follow-up.
    if text.contains("GP data has not updated") {
        log::info!("orbit: Celestrak rate-limited the TLE fetch; keeping existing catalog");
        return Vec::new();
    }
    // Collect once, walk by index — lets us peek at line1/line2
    // without consuming them when validation fails on a stray
    // "garbage" name line.
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let cur = lines[i];
        if let Some(stripped) = cur.strip_prefix("1 ") {
            // 2-line block (no name line). Require the next line
            // to be a valid `2 ` start; otherwise skip this `1 `.
            if i + 1 < lines.len() && lines[i + 1].starts_with("2 ") {
                let norad = stripped.split_whitespace().next().unwrap_or("0");
                let name = format!("NORAD {}", norad.trim_end_matches('U'));
                out.push(Tle {
                    name,
                    line1: cur.to_owned(),
                    line2: lines[i + 1].to_owned(),
                });
                i += 2;
                continue;
            }
        } else if i + 2 < lines.len()
            && lines[i + 1].starts_with("1 ")
            && lines[i + 2].starts_with("2 ")
        {
            // 3-line block: name, line1, line2.
            out.push(Tle {
                name: cur.to_owned(),
                line1: lines[i + 1].to_owned(),
                line2: lines[i + 2].to_owned(),
            });
            i += 3;
            continue;
        }
        // Not the start of any recognised block — advance one and
        // keep looking.
        i += 1;
    }
    out
}

/// Prepare `Satellite`s for propagation. TLEs that fail SGP4
/// constant initialisation (bad data, weird orbit) are skipped
/// with a warn-log; the rest pass through.
pub fn satellites_from_tles(tles: &[Tle], category: Category) -> Vec<Satellite> {
    let mut out = Vec::with_capacity(tles.len());
    for tle in tles {
        match prepare_one(tle, category) {
            Ok(sat) => out.push(sat),
            Err(e) => log::warn!("skip TLE {}: {e}", tle.name),
        }
    }
    out
}

fn prepare_one(tle: &Tle, category: Category) -> Result<Satellite, String> {
    let elements = Elements::from_tle(
        Some(tle.name.clone()),
        tle.line1.as_bytes(),
        tle.line2.as_bytes(),
    )
    .map_err(|e| format!("{e}"))?;
    let constants = Constants::from_elements(&elements).map_err(|e| format!("{e}"))?;
    let norad_id = elements.norad_id as u32;
    let epoch_unix_s = chrono_naive_to_unix(elements.datetime);
    Ok(Satellite {
        name: tle.name.clone(),
        norad_id,
        category,
        epoch_unix_s,
        elements,
        constants,
    })
}

fn chrono_naive_to_unix(dt: chrono::NaiveDateTime) -> f64 {
    // sgp4's `Elements::datetime` is a UTC naive timestamp.
    dt.and_utc().timestamp() as f64 + (dt.and_utc().timestamp_subsec_nanos() as f64) * 1e-9
}

// ---------------------------------------------------------------------------
// Propagation
// ---------------------------------------------------------------------------

/// Propagate `sat` to UTC time `sim_unix_s` (UNIX seconds). Returns
/// the position in **renderer body-fixed coordinates** with units
/// of Earth radii (so `length() ≈ 1.066` for the ISS at 410 km
/// altitude). Returns `None` if SGP4 fails (numerical breakdown,
/// far-future TLE, etc.).
///
/// The axis swap from standard ECEF (X toward 0°/0°, Z toward
/// north) to the renderer's body-fixed convention (Z toward 0°/0°,
/// Y toward north) is applied here.
pub fn propagate_render_space(sat: &Satellite, sim_unix_s: f64) -> Option<[f32; 3]> {
    let minutes_since_epoch = (sim_unix_s - sat.epoch_unix_s) / 60.0;
    let pred = sat
        .constants
        .propagate(sgp4::MinutesSinceEpoch(minutes_since_epoch))
        .ok()?;
    let teme = pred.position; // km, TEME
                              // TEME → ECEF: rotate by -GMST about Z.
    let theta = gmst_rad_from_unix(sim_unix_s);
    let (s, c) = theta.sin_cos();
    let ecef = [
        teme[0] * c + teme[1] * s,
        -teme[0] * s + teme[1] * c,
        teme[2],
    ];
    // ECEF → renderer body-fixed (prime meridian at +Z, north at +Y).
    // ECEF.x → render.z (prime meridian)
    // ECEF.y → render.x (east at prime meridian)
    // ECEF.z → render.y (north)
    let scale = 1.0 / EARTH_RADIUS_KM;
    Some([
        (ecef[1] * scale) as f32,
        (ecef[2] * scale) as f32,
        (ecef[0] * scale) as f32,
    ])
}

/// Greenwich Mean Sidereal Time in radians at UNIX time
/// `unix_s` (UTC). Used to spin TEME into ECEF.
///
/// Uses the standard Vallado low-order series (truncated to
/// the leading term) which is accurate to ~1 milliradian over a
/// few decades — well within the ~5 km visualization tolerance.
pub fn gmst_rad_from_unix(unix_s: f64) -> f64 {
    // Julian Date at the given UTC time. J2000 epoch is
    // 2451545.0 = 2000-01-01T12:00:00Z, which is UNIX
    // 946728000.0.
    const J2000_UNIX_S: f64 = 946_728_000.0;
    const SECS_PER_DAY: f64 = 86400.0;
    let d = (unix_s - J2000_UNIX_S) / SECS_PER_DAY; // days since J2000
                                                    // GMST in hours: 18.697374558 + 24.06570982441908 * d
    let gmst_hours = 18.697_374_558 + 24.065_709_824_419_08 * d;
    let gmst_rev = gmst_hours / 24.0;
    let frac = gmst_rev - gmst_rev.floor();
    frac * 2.0 * PI
}

/// Render-space position converted to `(lon°, lat°, alt_km)`. Used
/// by the M4 hover tooltip and orbit-track UI.
pub fn render_space_to_geodetic(pos: [f32; 3]) -> (f64, f64, f64) {
    let x = pos[0] as f64;
    let y = pos[1] as f64;
    let z = pos[2] as f64;
    let r = (x * x + y * y + z * z).sqrt();
    let lat = y.clamp(-1.0, 1.0).atan2((x * x + z * z).sqrt());
    let lon = x.atan2(z);
    let alt_km = (r - 1.0) * EARTH_RADIUS_KM;
    (lon.to_degrees(), lat.to_degrees(), alt_km)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const ISS_FIXTURE: &str = include_str!("../data/orbits/iss-fixture.txt");

    #[test]
    fn parses_iss_fixture() {
        let tles = parse_tles(ISS_FIXTURE);
        assert_eq!(tles.len(), 1);
        let t = &tles[0];
        assert!(t.name.contains("ISS"));
        assert!(t.line1.starts_with("1 25544"));
        assert!(t.line2.starts_with("2 25544"));
    }

    #[test]
    fn parses_two_line_blocks_without_name() {
        // Strip the name from the ISS fixture and check we still
        // load it — Celestrak's `gp.php` can return either format.
        let mut lines = ISS_FIXTURE.lines();
        let _ = lines.next();
        let two_line = lines.collect::<Vec<_>>().join("\n");
        let tles = parse_tles(&two_line);
        assert_eq!(tles.len(), 1);
        assert!(tles[0].name.contains("25544"));
    }

    #[test]
    fn skips_garbage_blocks() {
        let mixed = format!("garbage line\n{ISS_FIXTURE}\nmore garbage");
        let tles = parse_tles(&mixed);
        assert_eq!(tles.len(), 1);
    }

    #[test]
    fn iss_propagates_at_epoch() {
        let tles = parse_tles(ISS_FIXTURE);
        let sats = satellites_from_tles(&tles, Category::Stations);
        assert_eq!(sats.len(), 1);
        let iss = &sats[0];

        // At t = epoch the SGP4 propagation returns a non-zero
        // position; check the position vector magnitude is within
        // 100 km of the expected orbit radius (Earth radius + 410 km).
        let pos =
            propagate_render_space(iss, iss.epoch_unix_s).expect("propagation succeeds at epoch");
        let r = (pos[0].powi(2) + pos[1].powi(2) + pos[2].powi(2)).sqrt() as f64;
        let r_km = r * EARTH_RADIUS_KM;
        assert!(
            (6700.0..7000.0).contains(&r_km),
            "ISS orbit radius at epoch should be ~6788 km, got {r_km:.1}"
        );
    }

    #[test]
    fn iss_moves_over_one_minute() {
        let tles = parse_tles(ISS_FIXTURE);
        let sats = satellites_from_tles(&tles, Category::Stations);
        let iss = &sats[0];
        let p0 = propagate_render_space(iss, iss.epoch_unix_s).unwrap();
        let p1 = propagate_render_space(iss, iss.epoch_unix_s + 60.0).unwrap();
        let d = ((p0[0] - p1[0]).powi(2) + (p0[1] - p1[1]).powi(2) + (p0[2] - p1[2]).powi(2)).sqrt()
            as f64;
        // ISS travels ~7.66 km/s → ~459 km/min. In render units
        // (Earth radii) that's ~0.072.
        let d_render_units = d;
        assert!(
            (0.060..0.085).contains(&d_render_units),
            "ISS moves ~460 km in 60 s = ~0.072 render units; got {d_render_units}"
        );
    }

    #[test]
    fn trail_points_for_iss_form_a_ground_track_in_ecef() {
        let tles = parse_tles(ISS_FIXTURE);
        let sats = satellites_from_tles(&tles, Category::Stations);
        let iss = &sats[0];
        let points = iss.trail_points(iss.epoch_unix_s, 128);
        assert_eq!(points.len(), 128, "all sample points propagate");
        // Every point should be at roughly the same orbit radius
        // (LEO eccentricity is tiny — 0.0007 for the ISS — so the
        // orbit is nearly circular).
        let radii: Vec<f64> = points
            .iter()
            .map(|p| ((p[0] as f64).powi(2) + (p[1] as f64).powi(2) + (p[2] as f64).powi(2)).sqrt())
            .collect();
        let r_min = radii.iter().cloned().fold(f64::INFINITY, f64::min);
        let r_max = radii.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (r_max - r_min) / r_max < 0.02,
            "ISS orbit should be ~circular in ECEF; radii spread {r_min} → {r_max}"
        );
        // In *body-fixed* coords the trail is a ground-track spiral,
        // NOT a closed circle — Earth rotates ~23° during one ISS
        // orbital period. The first and last sample points (one
        // period apart in inertial time) sit on the same latitude
        // band but at different longitudes, separated by roughly
        // sin(23°)·orbit_radius ≈ 0.4 render units.
        let first = points.first().unwrap();
        let last = points.last().unwrap();
        let d = (((first[0] - last[0]).powi(2)
            + (first[1] - last[1]).powi(2)
            + (first[2] - last[2]).powi(2)) as f64)
            .sqrt();
        assert!(
            (0.2..0.7).contains(&d),
            "ISS trail endpoints should be ~0.4 render units apart in ECEF \
             (Earth rotates during the orbit); got {d:.3}"
        );
    }

    #[test]
    fn orbital_period_for_iss_is_about_93_minutes() {
        let tles = parse_tles(ISS_FIXTURE);
        let sats = satellites_from_tles(&tles, Category::Stations);
        let iss = &sats[0];
        let period = iss.orbital_period_minutes();
        assert!(
            (90.0..96.0).contains(&period),
            "ISS orbital period is ~93 min; got {period:.2}"
        );
    }

    #[test]
    fn gmst_at_j2000_epoch_matches_iau_reference() {
        // GMST at 2000-01-01T12:00:00Z (J2000) is 18.697374558 hours
        // (per Vallado / IAU); in radians that's ~4.894962.
        const J2000_UNIX_S: f64 = 946_728_000.0;
        let g = gmst_rad_from_unix(J2000_UNIX_S);
        let expected = 18.697_374_558 * 2.0 * PI / 24.0;
        let expected_wrapped = expected - (expected / (2.0 * PI)).floor() * 2.0 * PI;
        assert!(
            (g - expected_wrapped).abs() < 1e-6,
            "GMST at J2000: expected {expected_wrapped}, got {g}"
        );
    }

    #[test]
    fn gmst_advances_by_one_rotation_per_day() {
        // After one sidereal day (~23h 56m 4s) GMST returns nearly
        // to its starting value. After one *solar* day GMST advances
        // by ~4 minutes-of-arc more.
        let t0 = 1_700_000_000.0;
        let one_day_later = t0 + 86400.0;
        let g0 = gmst_rad_from_unix(t0);
        let g1 = gmst_rad_from_unix(one_day_later);
        // Difference should be ~2π * (1 + 1/365.25) ≈ 0.0172 rad
        // past one full revolution (which wraps to ~0.0172 + 0
        // when taken mod 2π).
        let diff_mod = ((g1 - g0) % (2.0 * PI) + 2.0 * PI) % (2.0 * PI);
        // diff_mod should be ~0.0172 (1/365.25 of a revolution)
        // — i.e. either very close to 0 or very close to 2π.
        let small = diff_mod.min(2.0 * PI - diff_mod);
        assert!(
            small < 0.03,
            "GMST should advance ~1 revolution per solar day; got diff_mod={diff_mod}"
        );
    }

    #[test]
    fn render_space_to_geodetic_round_trip_for_known_lonlat() {
        // Construct a render-space surface point at lon=−87.6°,
        // lat=41.9° (Chicago, sea level) and round-trip it.
        let lon_r: f64 = -87.6_f64.to_radians();
        let lat_r: f64 = 41.9_f64.to_radians();
        let p = [
            (lat_r.cos() * lon_r.sin()) as f32,
            lat_r.sin() as f32,
            (lat_r.cos() * lon_r.cos()) as f32,
        ];
        let (lon, lat, alt_km) = render_space_to_geodetic(p);
        assert!((lon - -87.6).abs() < 1e-4);
        assert!((lat - 41.9).abs() < 1e-4);
        assert!(alt_km.abs() < 1.0); // sea level ± 1 km
    }
}
