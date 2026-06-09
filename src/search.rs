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

// ---------------------------------------------------------------------------
// Geocoder client — turn a place-query string into a Vec of candidate
// SearchResults via Photon (primary) with Nominatim as a per-session
// fallback if Photon errors. Plan 0002 M1.
// ---------------------------------------------------------------------------

use crate::net;
use serde::Deserialize;
use thiserror::Error;

/// Categorical kind of a geocoder result. Drives the default fly-to
/// zoom when the result has no bounding box and the renderer
/// settles on a single point. Values calibrated against the typical
/// "feels right at this zoom" target across the test cities.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ResultKind {
    /// A country — render at a continental-scale zoom.
    Country,
    /// A state, province, or region — sub-country, multi-city.
    Region,
    /// A city or town.
    City,
    /// A street address or building.
    Address,
    /// A point of interest (museum, landmark, station, etc.).
    Poi,
    /// Anything Photon returns whose `osm_value` we don't recognise.
    Unknown,
}

impl ResultKind {
    /// Default flat-Mercator zoom for a result of this kind when no
    /// bounding box is available. Picked to frame the typical
    /// instance: a country fits in z=4, a city in z=12, a building
    /// in z=17. These are the values the M3 fly-to uses when the
    /// bbox-fit path isn't applicable.
    pub fn default_zoom(self) -> f64 {
        match self {
            ResultKind::Country => 4.0,
            ResultKind::Region => 6.0,
            ResultKind::City => 12.0,
            ResultKind::Address | ResultKind::Poi => 17.0,
            ResultKind::Unknown => 10.0,
        }
    }

    /// Map Photon's `osm_value` field (and a few other hints) to the
    /// project's kind enum. Photon tags follow OSM's
    /// `place=country / state / city / ...` taxonomy directly.
    fn from_photon(osm_key: &str, osm_value: &str) -> ResultKind {
        match (osm_key, osm_value) {
            ("place", "country") => ResultKind::Country,
            ("place", "state" | "region" | "province") => ResultKind::Region,
            ("place", "city" | "town" | "village" | "hamlet") => ResultKind::City,
            ("highway" | "building", _) => ResultKind::Address,
            ("tourism" | "amenity" | "leisure" | "historic" | "natural", _) => ResultKind::Poi,
            _ => ResultKind::Unknown,
        }
    }
}

/// One geocoder result mapped from a Photon (or Nominatim) feature.
/// Lon-first internally — same convention as the rest of the crate.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchResult {
    /// Short label, e.g. `"Topeka"`. Falls back to `osm_value`
    /// when Photon has no name.
    pub name: String,
    /// Human-readable context, e.g. `"Kansas, USA"`. Built from
    /// Photon's `state` / `country` / `city` fields, omitting the
    /// ones that duplicate `name`.
    pub context: String,
    /// `(lon, lat)` of the result's representative point.
    pub lonlat: (f64, f64),
    /// `(lon_min, lat_min, lon_max, lat_max)` if the geocoder
    /// returned one. Triggers the bbox-fit fly-to path in M3.
    pub bbox: Option<[f64; 4]>,
    pub kind: ResultKind,
}

/// Errors a geocoder call can produce. Each variant has a clear
/// caller policy: `Network` / `Unavailable` → retry with the
/// fallback (or surface "unreachable" to the user); `Empty` → show
/// "no matches"; `Decode` → genuine bug, flag in logs.
#[derive(Debug, Error)]
pub enum GeocodeError {
    #[error("network / transport: {0}")]
    Network(String),
    #[error("provider unavailable (HTTP {0})")]
    Unavailable(u16),
    #[error("response decode: {0}")]
    Decode(String),
    /// The query was malformed (empty after trim, etc.) — caller
    /// shouldn't have called geocode in the first place.
    #[error("malformed query")]
    Malformed,
}

impl From<net::NetError> for GeocodeError {
    fn from(e: net::NetError) -> GeocodeError {
        match e {
            net::NetError::Transport(s) => GeocodeError::Network(s),
            net::NetError::HttpStatus { status, .. } => GeocodeError::Unavailable(status),
        }
    }
}

/// Which geocoder backend a given call should target. Used by
/// `GeocoderClient` to remember a session-level switch from Photon
/// to Nominatim after the first Photon failure.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
enum Backend {
    #[default]
    Photon,
    Nominatim,
}

/// Stateful geocoder client. Holds the active backend so a single
/// Photon failure switches the rest of the session to Nominatim
/// rather than retrying Photon every keystroke. The struct exists
/// (rather than free functions) precisely so tests can swap in a
/// mock client to assert the debounce + fallback paths without
/// hitting the network.
#[derive(Clone, Debug, Default)]
pub struct GeocoderClient {
    backend: Backend,
}

impl GeocoderClient {
    pub fn new() -> GeocoderClient {
        GeocoderClient::default()
    }

    /// Which backend the next `geocode` call will hit.
    pub fn active_backend(&self) -> &'static str {
        match self.backend {
            Backend::Photon => "photon",
            Backend::Nominatim => "nominatim",
        }
    }

    /// URL the next `geocode` call will fetch for `query`. Public
    /// so tests can verify the URL shape and so the M2 UI can show
    /// it in a "request:" debug log if desired.
    pub fn url_for(&self, query: &str, near: Option<(f64, f64)>) -> String {
        let encoded = url_encode(query);
        match self.backend {
            Backend::Photon => {
                let mut url = format!("https://photon.komoot.io/api/?q={encoded}&limit=5&lang=en");
                if let Some((lon, lat)) = near {
                    use std::fmt::Write;
                    let _ = write!(&mut url, "&lat={lat}&lon={lon}");
                }
                url
            }
            Backend::Nominatim => format!(
                "https://nominatim.openstreetmap.org/search?q={encoded}&format=json&limit=5\
                 &addressdetails=1"
            ),
        }
    }
}

/// Percent-encode the query string for inclusion in a URL. Strictly
/// the subset of characters that matter for our geocoder URLs —
/// spaces, commas, and the small set of reserved characters that
/// could appear in a place name. Not a general-purpose encoder.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                let hi = b >> 4;
                let lo = b & 0x0f;
                fn nibble(n: u8) -> char {
                    if n < 10 {
                        (b'0' + n) as char
                    } else {
                        (b'A' + n - 10) as char
                    }
                }
                out.push('%');
                out.push(nibble(hi));
                out.push(nibble(lo));
            }
        }
    }
    out
}

// --- Photon response shape ------------------------------------------------

/// What Photon's `/api/?q=...` returns: a GeoJSON-ish
/// `FeatureCollection` where each feature's `geometry.coordinates`
/// is `[lon, lat]` and `properties` carries the place attributes.
#[derive(Deserialize)]
struct PhotonResponse {
    features: Vec<PhotonFeature>,
}

#[derive(Deserialize)]
struct PhotonFeature {
    geometry: PhotonGeometry,
    properties: PhotonProperties,
}

#[derive(Deserialize)]
struct PhotonGeometry {
    coordinates: [f64; 2], // [lon, lat]
}

#[derive(Deserialize, Default)]
struct PhotonProperties {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    osm_key: Option<String>,
    #[serde(default)]
    osm_value: Option<String>,
    /// Photon returns `extent` as `[w, n, e, s]` (longitude west,
    /// latitude NORTH, longitude east, latitude SOUTH) — note the
    /// north-before-south order, which is the opposite of the
    /// `[min_lon, min_lat, max_lon, max_lat]` shape `SearchResult`
    /// stores. The mapper re-orders.
    #[serde(default)]
    extent: Option<[f64; 4]>,
}

fn photon_features_to_results(features: Vec<PhotonFeature>) -> Vec<SearchResult> {
    features.into_iter().map(photon_feature_to_result).collect()
}

fn photon_feature_to_result(feat: PhotonFeature) -> SearchResult {
    let [lon, lat] = feat.geometry.coordinates;
    let osm_key = feat.properties.osm_key.as_deref().unwrap_or("");
    let osm_value = feat.properties.osm_value.as_deref().unwrap_or("");
    let kind = ResultKind::from_photon(osm_key, osm_value);
    let name = feat
        .properties
        .name
        .clone()
        .unwrap_or_else(|| osm_value.to_owned());
    let context = build_context(&feat.properties, &name);
    let bbox = feat.properties.extent.map(|[w, n, e, s]| [w, s, e, n]);
    SearchResult {
        name,
        context,
        lonlat: (lon, lat),
        bbox,
        kind,
    }
}

fn build_context(p: &PhotonProperties, name: &str) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(3);
    for field in [&p.city, &p.state, &p.country] {
        if let Some(s) = field.as_deref() {
            if !s.is_empty() && s != name && !parts.contains(&s) {
                parts.push(s);
            }
        }
    }
    parts.join(", ")
}

// --- Nominatim response shape --------------------------------------------

/// Nominatim's `format=json` returns a JSON array of place objects.
/// We map a minimal subset to `SearchResult` so the fallback path
/// surfaces useful results even when Photon is down.
#[derive(Deserialize)]
struct NominatimResult {
    lat: String,
    lon: String,
    display_name: String,
    #[serde(default)]
    class: Option<String>,
    #[serde(default, rename = "type")]
    type_: Option<String>,
    #[serde(default)]
    boundingbox: Option<[String; 4]>, // [lat_min, lat_max, lon_min, lon_max]
}

fn nominatim_results_to_results(raw: Vec<NominatimResult>) -> Vec<SearchResult> {
    raw.into_iter().filter_map(nominatim_to_result).collect()
}

fn nominatim_to_result(r: NominatimResult) -> Option<SearchResult> {
    let lat: f64 = r.lat.parse().ok()?;
    let lon: f64 = r.lon.parse().ok()?;
    let class = r.class.as_deref().unwrap_or("");
    let type_ = r.type_.as_deref().unwrap_or("");
    let kind = ResultKind::from_photon(class, type_); // Nominatim uses the same OSM tag values
    let (name, context) = match r.display_name.split_once(", ") {
        Some((n, rest)) => (n.to_owned(), rest.to_owned()),
        None => (r.display_name.clone(), String::new()),
    };
    let bbox = r
        .boundingbox
        .and_then(|[lat_min, lat_max, lon_min, lon_max]| {
            Some([
                lon_min.parse().ok()?,
                lat_min.parse().ok()?,
                lon_max.parse().ok()?,
                lat_max.parse().ok()?,
            ])
        });
    Some(SearchResult {
        name,
        context,
        lonlat: (lon, lat),
        bbox,
        kind,
    })
}

// --- Public geocode entry points -----------------------------------------

/// Native synchronous geocode. Blocks the calling thread; intended
/// for native worker threads or the M2 `Renderer::search_and_fly_to`
/// blocking API.
#[cfg(not(target_arch = "wasm32"))]
pub fn geocode_blocking(
    client: &mut GeocoderClient,
    query: &str,
    near: Option<(f64, f64)>,
) -> Result<Vec<SearchResult>, GeocodeError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(GeocodeError::Malformed);
    }

    // Try the active backend first.
    let url = client.url_for(trimmed, near);
    let result = net::fetch_bytes_blocking(&url).map_err(GeocodeError::from);

    match result {
        Ok(bytes) => decode_for_backend(client.backend, &bytes),
        Err(GeocodeError::Network(_)) | Err(GeocodeError::Unavailable(_))
            if client.backend == Backend::Photon =>
        {
            // First Photon failure of the session — switch to
            // Nominatim and try once more. Subsequent Photon
            // failures (if any) just propagate.
            client.backend = Backend::Nominatim;
            let url = client.url_for(trimmed, near);
            let bytes = net::fetch_bytes_blocking(&url).map_err(GeocodeError::from)?;
            decode_for_backend(client.backend, &bytes)
        }
        Err(other) => Err(other),
    }
}

/// Web async geocode. Same logic as the blocking variant; the
/// caller `.await`s the future. Implemented separately because the
/// native + web fetchers have different signatures.
#[cfg(target_arch = "wasm32")]
pub async fn geocode_async(
    client: &mut GeocoderClient,
    query: &str,
    near: Option<(f64, f64)>,
) -> Result<Vec<SearchResult>, GeocodeError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(GeocodeError::Malformed);
    }
    let url = client.url_for(trimmed, near);
    let bytes_result = net::fetch_bytes_async(&url)
        .await
        .map_err(GeocodeError::from);
    match bytes_result {
        Ok(bytes) => decode_for_backend(client.backend, &bytes),
        Err(GeocodeError::Network(_)) | Err(GeocodeError::Unavailable(_))
            if client.backend == Backend::Photon =>
        {
            client.backend = Backend::Nominatim;
            let url = client.url_for(trimmed, near);
            let bytes = net::fetch_bytes_async(&url)
                .await
                .map_err(GeocodeError::from)?;
            decode_for_backend(client.backend, &bytes)
        }
        Err(other) => Err(other),
    }
}

fn decode_for_backend(backend: Backend, bytes: &[u8]) -> Result<Vec<SearchResult>, GeocodeError> {
    match backend {
        Backend::Photon => {
            let resp: PhotonResponse =
                serde_json::from_slice(bytes).map_err(|e| GeocodeError::Decode(e.to_string()))?;
            Ok(photon_features_to_results(resp.features))
        }
        Backend::Nominatim => {
            let raw: Vec<NominatimResult> =
                serde_json::from_slice(bytes).map_err(|e| GeocodeError::Decode(e.to_string()))?;
            Ok(nominatim_results_to_results(raw))
        }
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

    // ---------------- Geocoder client ----------------

    #[test]
    fn url_encode_handles_spaces_and_commas() {
        assert_eq!(url_encode("Topeka, Kansas"), "Topeka%2C%20Kansas");
        assert_eq!(url_encode("chicago"), "chicago");
        assert_eq!(url_encode("São Paulo"), "S%C3%A3o%20Paulo");
    }

    #[test]
    fn photon_url_includes_near_when_provided() {
        let c = GeocoderClient::new();
        let with = c.url_for("Chicago", Some((-87.6, 41.9)));
        assert!(with.contains("lat=41.9"));
        assert!(with.contains("lon=-87.6"));
        let without = c.url_for("Chicago", None);
        assert!(!without.contains("lat="));
        assert!(!without.contains("lon="));
    }

    #[test]
    fn photon_url_targets_photon_by_default() {
        let c = GeocoderClient::new();
        assert!(c
            .url_for("X", None)
            .starts_with("https://photon.komoot.io/"));
        assert_eq!(c.active_backend(), "photon");
    }

    #[test]
    fn result_kind_default_zoom_ordering() {
        // The targets get progressively closer in.
        assert!(ResultKind::Country.default_zoom() < ResultKind::Region.default_zoom());
        assert!(ResultKind::Region.default_zoom() < ResultKind::City.default_zoom());
        assert!(ResultKind::City.default_zoom() < ResultKind::Poi.default_zoom());
    }

    #[test]
    fn result_kind_from_photon_recognises_common_osm_tags() {
        assert_eq!(
            ResultKind::from_photon("place", "country"),
            ResultKind::Country
        );
        assert_eq!(ResultKind::from_photon("place", "city"), ResultKind::City);
        assert_eq!(ResultKind::from_photon("place", "town"), ResultKind::City);
        assert_eq!(
            ResultKind::from_photon("tourism", "museum"),
            ResultKind::Poi
        );
        assert_eq!(
            ResultKind::from_photon("highway", "primary"),
            ResultKind::Address
        );
        assert_eq!(ResultKind::from_photon("xyz", "qux"), ResultKind::Unknown);
    }

    #[test]
    fn photon_decode_round_trips_a_real_response() {
        // A minimal but realistic Photon response shape — one
        // city feature with an extent. Pinning the decode here
        // means a regression in serde-derives or the field rename
        // game is caught without hitting the network.
        let json = br#"{
            "features": [{
                "geometry": {"type": "Point", "coordinates": [-95.6, 39.05]},
                "properties": {
                    "osm_id": 12345,
                    "osm_key": "place",
                    "osm_value": "city",
                    "name": "Topeka",
                    "state": "Kansas",
                    "country": "United States",
                    "extent": [-95.95, 39.15, -95.5, 38.95]
                }
            }]
        }"#;
        let results = decode_for_backend(Backend::Photon, json).expect("decodes");
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.name, "Topeka");
        assert_eq!(r.kind, ResultKind::City);
        assert_eq!(r.context, "Kansas, United States");
        assert!((r.lonlat.0 + 95.6).abs() < 1e-9);
        assert!((r.lonlat.1 - 39.05).abs() < 1e-9);
        // Photon's extent is [w, n, e, s]; SearchResult stores
        // [lon_min, lat_min, lon_max, lat_max] so n/s swap.
        let bbox = r.bbox.expect("has bbox");
        assert!((bbox[1] - 38.95).abs() < 1e-9, "lat_min should be 38.95");
        assert!((bbox[3] - 39.15).abs() < 1e-9, "lat_max should be 39.15");
    }

    #[test]
    fn nominatim_decode_round_trips_a_real_response() {
        let json = br#"[{
            "lat": "39.05",
            "lon": "-95.6",
            "display_name": "Topeka, Shawnee County, Kansas, USA",
            "class": "place",
            "type": "city",
            "boundingbox": ["38.95", "39.15", "-95.95", "-95.5"]
        }]"#;
        let results = decode_for_backend(Backend::Nominatim, json).expect("decodes");
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.name, "Topeka");
        assert_eq!(r.kind, ResultKind::City);
        assert_eq!(r.context, "Shawnee County, Kansas, USA");
    }

    // The live-network integration test is `#[ignore]` so it runs
    // only with `cargo test -- --ignored`. Hitting the public
    // Photon instance on every CI run would be a bad neighbour.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    #[ignore]
    fn geocode_chicago_native_hits_photon() {
        let mut client = GeocoderClient::new();
        let results = geocode_blocking(&mut client, "Chicago", None).expect("photon up");
        let first = results.first().expect("at least one result");
        assert_eq!(first.name, "Chicago");
        assert_eq!(first.kind, ResultKind::City);
        let (lon, lat) = first.lonlat;
        assert!(
            (lon - -87.7).abs() < 0.5,
            "lon: got {lon}, expected -87.7 ± 0.5"
        );
        assert!(
            (lat - 41.9).abs() < 0.5,
            "lat: got {lat}, expected 41.9 ± 0.5"
        );
    }
}
