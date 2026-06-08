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
