//! Search: turn a typed query into a `(lon, lat)` and optional
//! target zoom that the renderer's fly-to can consume.
//!
//! Two input paths share one entry surface:
//!
//! - **Coordinate expressions** (`"41.87, -87.63"`, `"41°52′N
//!   87°37′W"`, etc.) — parsed offline by [`parse_coord`]. No
//!   network. Plan 0002 M0.
//! - **Place queries** (`"Topeka, Kansas"`) — routed to an open
//!   geocoder (Photon, with Nominatim as a per-session fallback).
//!   Plan 0002 M1.
//!
//! The convention everywhere in aeGIS is **(lon, lat)** in that
//! order. Coord-parser inputs are usually `(lat, lon)` (the
//! human-facing convention); the parser converts at the
//! boundary so the rest of the codebase doesn't have to think
//! about it.

use std::sync::OnceLock;

use regex::Regex;

/// Parse a coordinate expression into `(lon, lat)` if the input
/// matches one of the supported formats. Returns `None` for any
/// input that doesn't parse or whose values fall outside the
/// valid lat/lon ranges (so a typo doesn't silently fly the
/// camera to nowhere).
///
/// Supported formats, tried in order most-specific-first:
///
/// 1. **DMS** (degrees-minutes-seconds with hemisphere letters):
///    `"41°52'12\"N 87°37'48\"W"`. Unicode prime/double-prime
///    (`′`/`″`) and ASCII apostrophe/quote (`'`/`"`) both accepted;
///    the degree symbol is optional.
/// 2. **Decimal with hemisphere letters**:
///    `"41.87°N 87.63°W"`, `"41.87N, 87.63W"`. Hemisphere letter
///    fixes the sign.
/// 3. **Bare decimal pair** (comma- or whitespace-separated):
///    `"41.87, -87.63"`. Assumes (lat, lon) — the human-facing
///    convention. If the first value is outside `[-90, 90]` we
///    assume the user typed (lon, lat) instead and swap.
///
/// All paths return `(lon, lat)` (lon first — the aeGIS internal
/// convention). Out-of-range values (`|lat| > 90` or
/// `|lon| > 180`) yield `None`.
pub fn parse_coord(input: &str) -> Option<(f64, f64)> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    parse_dms(s)
        .or_else(|| parse_decimal_with_hemisphere(s))
        .or_else(|| parse_bare_decimal(s))
        .filter(|&(lon, lat)| (-180.0..=180.0).contains(&lon) && (-90.0..=90.0).contains(&lat))
}

/// DMS form, e.g. `41°52'12"N 87°37'48"W`. Order is (lat, lon).
fn parse_dms(s: &str) -> Option<(f64, f64)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        // Two DMS triples separated by whitespace and/or comma.
        // Degrees: integer or decimal. Minutes: integer or decimal.
        // Seconds: integer or decimal. The degree, prime, double-
        // prime, and comma separators are all optional / unicode-
        // flexible. Hemisphere letter is mandatory (without it
        // we'd be guessing sign + axis from raw degrees, which is
        // exactly what the decimal-with-hemisphere path handles
        // less ambiguously).
        // Verbose mode (?x) strips whitespace from inside character
        // classes too, which breaks `[, \t]` as a separator class —
        // hence the inline single-line regex.
        Regex::new(
            r#"^(?P<d1>[0-9]+(?:\.[0-9]+)?)[ \t]*°?[ \t]*(?:(?P<m1>[0-9]+(?:\.[0-9]+)?)[ \t]*['′][ \t]*)?(?:(?P<s1>[0-9]+(?:\.[0-9]+)?)[ \t]*["″][ \t]*)?(?P<h1>[NSns])[ \t]*[, \t][ \t]*(?P<d2>[0-9]+(?:\.[0-9]+)?)[ \t]*°?[ \t]*(?:(?P<m2>[0-9]+(?:\.[0-9]+)?)[ \t]*['′][ \t]*)?(?:(?P<s2>[0-9]+(?:\.[0-9]+)?)[ \t]*["″][ \t]*)?(?P<h2>[EWew])$"#,
        )
        .expect("DMS regex compiles")
    });
    let caps = re.captures(s)?;
    let lat = dms_to_decimal(&caps["d1"], caps.name("m1"), caps.name("s1"), &caps["h1"])?;
    let lon = dms_to_decimal(&caps["d2"], caps.name("m2"), caps.name("s2"), &caps["h2"])?;
    Some((lon, lat))
}

fn dms_to_decimal(
    d: &str,
    m: Option<regex::Match>,
    sec: Option<regex::Match>,
    hemi: &str,
) -> Option<f64> {
    let degrees: f64 = d.parse().ok()?;
    let minutes: f64 = m.map_or(Ok(0.0), |x| x.as_str().parse()).ok()?;
    let seconds: f64 = sec.map_or(Ok(0.0), |x| x.as_str().parse()).ok()?;
    let mag = degrees + minutes / 60.0 + seconds / 3600.0;
    let sign = match hemi {
        "N" | "n" | "E" | "e" => 1.0,
        "S" | "s" | "W" | "w" => -1.0,
        _ => return None,
    };
    Some(sign * mag)
}

/// Decimal pair with explicit hemisphere letters, e.g.
/// `41.87°N, 87.63°W` or `41.87N 87.63W`. Order is (lat, lon).
fn parse_decimal_with_hemisphere(s: &str) -> Option<(f64, f64)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r#"^(?P<v1>-?[0-9]+(?:\.[0-9]+)?)[ \t]*°?[ \t]*(?P<h1>[NSns])[ \t]*[, \t][ \t]*(?P<v2>-?[0-9]+(?:\.[0-9]+)?)[ \t]*°?[ \t]*(?P<h2>[EWew])$"#,
        )
        .expect("decimal-with-hemisphere regex compiles")
    });
    let caps = re.captures(s)?;
    let v1: f64 = caps["v1"].parse().ok()?;
    let v2: f64 = caps["v2"].parse().ok()?;
    let lat_sign = if caps["h1"].eq_ignore_ascii_case("S") {
        -1.0
    } else {
        1.0
    };
    let lon_sign = if caps["h2"].eq_ignore_ascii_case("W") {
        -1.0
    } else {
        1.0
    };
    Some((lon_sign * v2.abs(), lat_sign * v1.abs()))
}

/// Bare `"<a>, <b>"` or `"<a> <b>"` pair of decimal numbers.
/// Assumes (lat, lon) per the human-facing convention. If the
/// first value is out of the latitude range `[-90, 90]`, assume
/// the user typed (lon, lat) — common enough among GIS folks
/// that silently swapping is more useful than rejecting.
///
/// **Inherent ambiguity:** when both values are in lat range
/// (e.g. Chicago `-87.63, 41.87` — both in `[-90, 90]`), there
/// is no way to know which axis the user meant. We commit to
/// (lat, lon), so a lon-first Chicago input would interpret as
/// (lat=-87.63, lon=41.87) and land in Antarctica. Users who
/// want lon-first for ambiguous inputs should use the
/// hemisphere-letter form, which is unambiguous.
fn parse_bare_decimal(s: &str) -> Option<(f64, f64)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r#"^(?P<v1>-?[0-9]+(?:\.[0-9]+)?)[ \t]*[, \t][ \t]*(?P<v2>-?[0-9]+(?:\.[0-9]+)?)$"#,
        )
        .expect("bare-decimal regex compiles")
    });
    let caps = re.captures(s)?;
    let v1: f64 = caps["v1"].parse().ok()?;
    let v2: f64 = caps["v2"].parse().ok()?;
    // First value out of lat range → user typed (lon, lat) order.
    if !(-90.0..=90.0).contains(&v1) {
        Some((v1, v2))
    } else {
        Some((v2, v1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    fn assert_lonlat(parsed: Option<(f64, f64)>, expected: (f64, f64), eps: f64) {
        let (lon, lat) = parsed.expect("parse succeeded");
        let (exp_lon, exp_lat) = expected;
        assert!(
            close(lon, exp_lon, eps),
            "lon: got {lon}, expected {exp_lon}"
        );
        assert!(
            close(lat, exp_lat, eps),
            "lat: got {lat}, expected {exp_lat}"
        );
    }

    // ---------------- Bare decimal pair ----------------

    #[test]
    fn parses_bare_decimal_lat_lon() {
        // Chicago — typed (lat, lon).
        assert_lonlat(parse_coord("41.87, -87.63"), (-87.63, 41.87), 1e-9);
    }

    #[test]
    fn parses_bare_decimal_whitespace_separated() {
        assert_lonlat(parse_coord("41.87 -87.63"), (-87.63, 41.87), 1e-9);
    }

    #[test]
    fn bare_decimal_first_value_out_of_lat_range_implies_lon_first() {
        // First value 151.2 can't be a latitude (out of [-90, 90]),
        // so we interpret as (lon, lat). Sydney.
        assert_lonlat(parse_coord("151.2, -33.9"), (151.2, -33.9), 1e-9);
    }

    // ---------------- Decimal with hemisphere ----------------

    #[test]
    fn parses_decimal_with_hemisphere_letters() {
        // Chicago, hemisphere-letter form.
        assert_lonlat(parse_coord("41.87°N 87.63°W"), (-87.63, 41.87), 1e-9);
    }

    #[test]
    fn parses_decimal_with_hemisphere_comma_separated() {
        assert_lonlat(parse_coord("41.87N, 87.63W"), (-87.63, 41.87), 1e-9);
    }

    #[test]
    fn parses_decimal_with_hemisphere_south_east() {
        // Sydney, in the south + east.
        assert_lonlat(parse_coord("33.9S, 151.2E"), (151.2, -33.9), 1e-9);
    }

    // ---------------- DMS ----------------

    #[test]
    fn parses_dms_with_ascii_quotes() {
        // Chicago in DMS, ASCII apostrophes + quotes.
        assert_lonlat(
            parse_coord("41°52'12\"N 87°37'48\"W"),
            (-87.63, 41.87),
            0.01,
        );
    }

    #[test]
    fn parses_dms_with_unicode_primes() {
        assert_lonlat(parse_coord("41°52′12″N 87°37′48″W"), (-87.63, 41.87), 0.01);
    }

    #[test]
    fn parses_dms_without_seconds() {
        // Just degrees + minutes still works.
        assert_lonlat(parse_coord("41°52'N 87°38'W"), (-87.63, 41.87), 0.02);
    }

    // ---------------- Rejection ----------------

    #[test]
    fn rejects_out_of_range_lat_with_hemisphere() {
        // 95° N with an explicit hemisphere letter is unambiguous —
        // user meant latitude and 95° is out of range. Bare-decimal
        // `"95.0, -87.63"` is ambiguous (could be lon-first) and the
        // parser commits to the lon-first interpretation per its
        // documented heuristic; tested in
        // `bare_decimal_first_value_out_of_lat_range_implies_lon_first`.
        assert_eq!(parse_coord("95.0°N 87.63°W"), None);
    }

    #[test]
    fn rejects_out_of_range_lon() {
        // 200° is outside [-180, 180].
        assert_eq!(parse_coord("41.87, 200.0"), None);
        assert_eq!(parse_coord("41.87°N 200.0°E"), None);
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_coord("xyzzy"), None);
        assert_eq!(parse_coord(""), None);
        assert_eq!(parse_coord("   "), None);
        assert_eq!(parse_coord("41.87"), None);
        assert_eq!(parse_coord("hello, world"), None);
    }

    // ---------------- City round-trip ----------------

    /// Each test city expressed in every supported format must
    /// parse to within `1e-2°` of its truth `(lon, lat)`. That's
    /// well under one tile span at every reasonable zoom, so any
    /// of these inputs lands the camera in the same tile.
    #[test]
    fn city_round_trip_chicago() {
        // Chicago: both lat and lon are in [-90, 90], so the
        // bare-decimal lon-first form is ambiguous and not
        // exercised here — see `parse_bare_decimal`'s doc comment.
        let truth = (-87.63, 41.87);
        let inputs = [
            "41.87, -87.63",
            "41.87°N, 87.63°W",
            "41°52'12\"N 87°37'48\"W",
        ];
        for input in inputs {
            assert_lonlat(parse_coord(input), truth, 0.01);
        }
    }

    #[test]
    fn city_round_trip_sydney() {
        let truth = (151.2, -33.9);
        let inputs = [
            "-33.9, 151.2",
            "151.2, -33.9", // lon-first (first value out of lat range)
            "33.9°S, 151.2°E",
            "33°54'S 151°12'E",
        ];
        for input in inputs {
            assert_lonlat(parse_coord(input), truth, 0.01);
        }
    }

    #[test]
    fn city_round_trip_reykjavik() {
        let truth = (-21.94, 64.13);
        let inputs = [
            "64.13, -21.94",
            "64.13°N, 21.94°W",
            "64°7'48\"N 21°56'24\"W",
        ];
        for input in inputs {
            assert_lonlat(parse_coord(input), truth, 0.01);
        }
    }

    #[test]
    fn city_round_trip_tokyo() {
        let truth = (139.69, 35.69);
        let inputs = [
            "35.69, 139.69",
            "139.69, 35.69", // lon-first
            "35.69°N, 139.69°E",
            "35°41'24\"N 139°41'24\"E",
        ];
        for input in inputs {
            assert_lonlat(parse_coord(input), truth, 0.01);
        }
    }
}
