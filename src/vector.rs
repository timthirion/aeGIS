//! GeoJSON ingest + vector-layer construction for the M3 overlay.
//!
//! Loads a GeoJSON `FeatureCollection` (countries, coastlines,
//! whatever the caller supplies), walks the geometry tree, and
//! collapses every line + polygon ring into a flat `Vec<[f32; 2]>`
//! of vertices in **normalised Mercator world coords** (the same
//! `[0, 1] × [0, 1]` space `core::crs` uses).
//!
//! The output is laid out as a wgpu `LineList` would consume it —
//! every pair of consecutive vertices is one line segment. Closed
//! rings include the final segment back to the start.
//!
//! The vertex shader (`shaders/vector.wgsl`) projects each vertex
//! through the camera uniform; the projection is the single point
//! we'll swap when the globe view lands (the data stays the same,
//! the shader interpolates from flat Mercator to ellipsoidal).

use geojson::{GeoJson, Geometry, Value};

use crate::crs;

/// One feature in the identify index — its display name + the
/// (lon, lat) polygon footprint(s) used for point-in-polygon
/// hit-testing. Plan 0007.
#[derive(Debug, Clone)]
pub struct IdentifyFeature {
    pub name: String,
    /// Axis-aligned bounding box in degrees: `[min_lon, min_lat,
    /// max_lon, max_lat]`. Pre-check before the polygon test so a
    /// click only pays the O(ring vertices) cost for plausible
    /// candidates.
    pub bbox: [f64; 4],
    /// Multipolygon: outer slice is the polygons, inner is the
    /// rings of one polygon (ring 0 = outer, rings 1+ = holes),
    /// inner-most is `(lon, lat)` pairs in degrees.
    pub polygons: Vec<Vec<Vec<[f64; 2]>>>,
}

/// All features in the loaded GeoJSON paired with their click-
/// hit-test footprints. Built alongside the line-list `VectorLayer`
/// in [`load_geojson`]; the renderer holds both so a click can
/// answer "what country / feature is under the cursor."
#[derive(Debug, Clone, Default)]
pub struct IdentifyIndex {
    pub features: Vec<IdentifyFeature>,
}

impl IdentifyIndex {
    /// Find the feature whose polygons contain `(lon, lat)`.
    /// Linear scan with a bbox pre-check; at the Natural Earth
    /// 110 m scale (250 features) this is microsecond-scale even
    /// for a few-ring island country, fast enough that an rstar
    /// R-tree (plan 0007's stretch goal) would be premature.
    pub fn pick(&self, lon: f64, lat: f64) -> Option<&IdentifyFeature> {
        for f in &self.features {
            if lon < f.bbox[0] || lon > f.bbox[2] || lat < f.bbox[1] || lat > f.bbox[3] {
                continue;
            }
            if point_in_multipolygon((lon, lat), &f.polygons) {
                return Some(f);
            }
        }
        None
    }
}

/// Even-odd point-in-polygon test for a multipolygon. Counts ring
/// crossings; a point is "inside" iff the count is odd across all
/// rings of any one polygon. Antimeridian wrap is *not* handled —
/// Natural Earth's 110 m polygons that span ±180° are pre-split,
/// so a naive ray cast works.
fn point_in_multipolygon(p: (f64, f64), polygons: &[Vec<Vec<[f64; 2]>>]) -> bool {
    for poly in polygons {
        let mut inside = false;
        for ring in poly {
            if point_in_ring(p, ring) {
                inside = !inside;
            }
        }
        if inside {
            return true;
        }
    }
    false
}

fn point_in_ring(p: (f64, f64), ring: &[[f64; 2]]) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let (px, py) = p;
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (xi, yi) = (ring[i][0], ring[i][1]);
        let (xj, yj) = (ring[j][0], ring[j][1]);
        if ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// A vector overlay ready for upload to the GPU as a `LineList`.
#[derive(Debug, Clone, Default)]
pub struct VectorLayer {
    /// Vertices in normalised Mercator `(x, y)`. Every consecutive
    /// pair `[v[2i], v[2i+1]]` is one line segment.
    pub vertices: Vec<[f32; 2]>,
}

impl VectorLayer {
    /// Number of line segments in this layer (== `vertices.len() / 2`).
    pub fn segment_count(&self) -> usize {
        self.vertices.len() / 2
    }
}

/// Errors that can come out of [`load_geojson_lines`]. `geojson::Error`
/// is large (~200 bytes), so it's boxed to keep `Result<_, LoadError>`
/// pointer-sized.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("parse: {0}")]
    Parse(#[from] Box<geojson::Error>),
}

impl From<geojson::Error> for LoadError {
    fn from(e: geojson::Error) -> Self {
        LoadError::Parse(Box::new(e))
    }
}

/// Parse a GeoJSON string and produce a [`VectorLayer`] containing
/// every linestring + polygon-ring as `LineList`-ready segments. Each
/// `(lon, lat)` coordinate is projected through Spherical Mercator
/// (clamped to the projection's defined latitude band).
///
/// Unrecognised geometry types (`Point`, `MultiPoint`) are silently
/// skipped — they don't map cleanly to a line-only renderer.
pub fn load_geojson_lines(source: &str) -> Result<VectorLayer, LoadError> {
    let geojson: GeoJson = source.parse()?;
    let mut layer = VectorLayer::default();
    walk(&geojson, &mut layer);
    Ok(layer)
}

/// Load a GeoJSON source *both* as a line-list `VectorLayer` (for
/// the existing outline pipeline) *and* as an `IdentifyIndex` (for
/// click-to-identify hit-testing). Walks the feature tree once,
/// emitting projected line vertices and unprojected polygon
/// footprints in parallel. Plan 0007.
pub fn load_geojson(source: &str) -> Result<(VectorLayer, IdentifyIndex), LoadError> {
    let geojson: GeoJson = source.parse()?;
    let mut layer = VectorLayer::default();
    let mut index = IdentifyIndex::default();
    walk_with_identify(&geojson, &mut layer, &mut index);
    Ok((layer, index))
}

fn walk_with_identify(g: &GeoJson, out: &mut VectorLayer, idx: &mut IdentifyIndex) {
    match g {
        GeoJson::FeatureCollection(fc) => {
            for f in &fc.features {
                if let Some(geom) = &f.geometry {
                    walk_geometry(geom, out);
                    if let Some(identify) = feature_to_identify(f, geom) {
                        idx.features.push(identify);
                    }
                }
            }
        }
        GeoJson::Feature(f) => {
            if let Some(geom) = &f.geometry {
                walk_geometry(geom, out);
                if let Some(identify) = feature_to_identify(f, geom) {
                    idx.features.push(identify);
                }
            }
        }
        GeoJson::Geometry(geom) => walk_geometry(geom, out),
    }
}

fn feature_to_identify(f: &geojson::Feature, geom: &Geometry) -> Option<IdentifyFeature> {
    let name = feature_display_name(f)?;
    let polygons = extract_polygons(geom);
    if polygons.is_empty() {
        return None;
    }
    let bbox = polygons_bbox(&polygons);
    Some(IdentifyFeature {
        name,
        bbox,
        polygons,
    })
}

/// Pick a human-readable name from the feature's properties.
/// Natural Earth files use uppercase `NAME` / `NAME_LONG`; other
/// providers may use lowercase. Returns the first non-empty one,
/// then `None` if nothing usable is present.
fn feature_display_name(f: &geojson::Feature) -> Option<String> {
    let props = f.properties.as_ref()?;
    for key in [
        "NAME",
        "name",
        "NAME_LONG",
        "ADMIN",
        "SOVEREIGNT",
        "FORMAL_EN",
    ] {
        if let Some(v) = props.get(key) {
            if let Some(s) = v.as_str() {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

fn extract_polygons(geom: &Geometry) -> Vec<Vec<Vec<[f64; 2]>>> {
    let mut out = Vec::new();
    extract_polygons_into(geom, &mut out);
    out
}

fn extract_polygons_into(geom: &Geometry, out: &mut Vec<Vec<Vec<[f64; 2]>>>) {
    match &geom.value {
        Value::Polygon(rings) => {
            let mut poly = Vec::with_capacity(rings.len());
            for ring in rings {
                let coords: Vec<[f64; 2]> = ring
                    .iter()
                    .filter_map(|c| {
                        if c.len() >= 2 {
                            Some([c[0], c[1]])
                        } else {
                            None
                        }
                    })
                    .collect();
                if !coords.is_empty() {
                    poly.push(coords);
                }
            }
            if !poly.is_empty() {
                out.push(poly);
            }
        }
        Value::MultiPolygon(polys) => {
            for rings in polys {
                let mut poly = Vec::with_capacity(rings.len());
                for ring in rings {
                    let coords: Vec<[f64; 2]> = ring
                        .iter()
                        .filter_map(|c| {
                            if c.len() >= 2 {
                                Some([c[0], c[1]])
                            } else {
                                None
                            }
                        })
                        .collect();
                    if !coords.is_empty() {
                        poly.push(coords);
                    }
                }
                if !poly.is_empty() {
                    out.push(poly);
                }
            }
        }
        Value::GeometryCollection(children) => {
            for child in children {
                extract_polygons_into(child, out);
            }
        }
        _ => {}
    }
}

fn polygons_bbox(polygons: &[Vec<Vec<[f64; 2]>>]) -> [f64; 4] {
    let mut min_lon = f64::INFINITY;
    let mut min_lat = f64::INFINITY;
    let mut max_lon = f64::NEG_INFINITY;
    let mut max_lat = f64::NEG_INFINITY;
    for poly in polygons {
        for ring in poly {
            for c in ring {
                min_lon = min_lon.min(c[0]);
                max_lon = max_lon.max(c[0]);
                min_lat = min_lat.min(c[1]);
                max_lat = max_lat.max(c[1]);
            }
        }
    }
    [min_lon, min_lat, max_lon, max_lat]
}

fn walk(g: &GeoJson, out: &mut VectorLayer) {
    match g {
        GeoJson::FeatureCollection(fc) => {
            for f in &fc.features {
                if let Some(geom) = &f.geometry {
                    walk_geometry(geom, out);
                }
            }
        }
        GeoJson::Feature(f) => {
            if let Some(geom) = &f.geometry {
                walk_geometry(geom, out);
            }
        }
        GeoJson::Geometry(geom) => walk_geometry(geom, out),
    }
}

fn walk_geometry(geom: &Geometry, out: &mut VectorLayer) {
    match &geom.value {
        Value::LineString(coords) => push_polyline(coords, false, out),
        Value::MultiLineString(lines) => {
            for line in lines {
                push_polyline(line, false, out);
            }
        }
        Value::Polygon(rings) => {
            for ring in rings {
                // Polygon rings are closed by GeoJSON convention; if
                // the first/last coords aren't identical, our `close`
                // pass adds the missing segment.
                push_polyline(ring, true, out);
            }
        }
        Value::MultiPolygon(polys) => {
            for rings in polys {
                for ring in rings {
                    push_polyline(ring, true, out);
                }
            }
        }
        Value::GeometryCollection(children) => {
            for child in children {
                walk_geometry(child, out);
            }
        }
        // Points / MultiPoints aren't a fit for a line-only renderer.
        Value::Point(_) | Value::MultiPoint(_) => {}
    }
}

/// Project each `(lon, lat)` pair to Mercator world and emit
/// `LineList`-style segment pairs into `out`. If `closed` is true and
/// the ring isn't already closed in the source, a final segment from
/// the last vertex back to the first is appended.
fn push_polyline(coords: &[Vec<f64>], closed: bool, out: &mut VectorLayer) {
    if coords.len() < 2 {
        return;
    }
    let project = |c: &[f64]| {
        let (wx, wy) = crs::lonlat_to_world(c[0], c[1]);
        [wx as f32, wy as f32]
    };
    let projected: Vec<[f32; 2]> = coords
        .iter()
        .filter(|c| c.len() >= 2)
        .map(|c| project(c))
        .collect();
    if projected.len() < 2 {
        return;
    }
    for pair in projected.windows(2) {
        out.vertices.push(pair[0]);
        out.vertices.push(pair[1]);
    }
    if closed {
        let first = projected[0];
        let last = *projected.last().unwrap();
        // Floats from GeoJSON `[lon, lat]` round-trip cleanly through
        // `f32`-Mercator, so the bit-pattern equality check below is
        // safe for "is this ring already closed in the source?".
        if first != last {
            out.vertices.push(last);
            out.vertices.push(first);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_collection_yields_empty_layer() {
        let src = r#"{ "type": "FeatureCollection", "features": [] }"#;
        let layer = load_geojson_lines(src).unwrap();
        assert!(layer.vertices.is_empty());
        assert_eq!(layer.segment_count(), 0);
    }

    #[test]
    fn linestring_becomes_n_minus_1_segments() {
        // Three points → two segments (A-B, B-C).
        let src = r#"{
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {
                    "type": "LineString",
                    "coordinates": [[-87.6298, 41.8781], [0.0, 0.0], [139.6917, 35.6895]]
                },
                "properties": {}
            }]
        }"#;
        let layer = load_geojson_lines(src).unwrap();
        assert_eq!(layer.segment_count(), 2);
        assert_eq!(layer.vertices.len(), 4);
    }

    #[test]
    fn open_polygon_ring_is_closed_on_emit() {
        // 4 distinct points (open ring) → 4 segments (3 source edges
        // + 1 close-the-ring edge).
        let src = r#"{
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {
                    "type": "Polygon",
                    "coordinates": [[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]]
                },
                "properties": {}
            }]
        }"#;
        let layer = load_geojson_lines(src).unwrap();
        assert_eq!(layer.segment_count(), 4);
    }

    #[test]
    fn closed_polygon_ring_emits_no_extra_close() {
        // 4 points with the first repeated at the end → 4 source
        // edges, no extra close.
        let src = r#"{
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {
                    "type": "Polygon",
                    "coordinates": [[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0], [0.0, 0.0]]]
                },
                "properties": {}
            }]
        }"#;
        let layer = load_geojson_lines(src).unwrap();
        // 5 source coords → 4 windows of 2, no extra close (first == last).
        assert_eq!(layer.segment_count(), 4);
    }

    #[test]
    fn coords_project_through_spherical_mercator() {
        // Equator + prime meridian → world centre (0.5, 0.5).
        let src = r#"{
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {
                    "type": "LineString",
                    "coordinates": [[0.0, 0.0], [0.0, 0.0]]
                },
                "properties": {}
            }]
        }"#;
        let layer = load_geojson_lines(src).unwrap();
        let v = layer.vertices[0];
        assert!((v[0] - 0.5).abs() < 1e-6, "x = {}", v[0]);
        assert!((v[1] - 0.5).abs() < 1e-6, "y = {}", v[1]);
    }

    #[test]
    fn skips_points_and_multipoints() {
        let src = r#"{
            "type": "FeatureCollection",
            "features": [
                { "type": "Feature", "geometry": { "type": "Point", "coordinates": [0, 0] }, "properties": {} },
                { "type": "Feature", "geometry": { "type": "MultiPoint", "coordinates": [[0, 0], [1, 1]] }, "properties": {} }
            ]
        }"#;
        let layer = load_geojson_lines(src).unwrap();
        assert_eq!(layer.segment_count(), 0);
    }
}
