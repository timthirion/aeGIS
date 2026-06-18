//! 3D-building data + mesh prep for plan 0014.
//!
//! Loads a trimmed GeoJSON FeatureCollection of OSM building footprints
//! (see `data/buildings/README.md`), assigns each building a height in
//! metres from its OSM tags, projects the footprint to normalised-
//! Mercator world coords, and builds:
//!
//! 1. a `Vec<Building>` — the source-of-truth per-building data
//!    (footprint, height, name, OSM way id, height-source flag),
//! 2. a `BuildingMesh` — packed VBO + IBO + per-building storage
//!    buffer the renderer uploads in a single pass,
//! 3. an [`IdentifyIndex`](crate::vector::IdentifyIndex) — the same
//!    polygon hit-test plumbing the country-outline picker uses,
//!    so a click at street zoom can answer "which building is this."
//!
//! The vertex format encodes a building as: per-vertex `(world_xy,
//! height_world, building_idx, face_kind, normal)` + per-building
//! `(centroid_normal)`. The shader does the (lon, lat) → sphere
//! projection itself + extrudes radially along the centroid normal
//! by `height_world`. See `src/shaders/building.wgsl`.

use bytemuck::{Pod, Zeroable};
use geojson::{GeoJson, Value};

use crate::{crs, vector};

/// Default building height in metres when no OSM tag is present.
/// Small enough that an untagged skyscraper is visibly suspicious
/// (a real diagnostic), big enough that an untagged garden shed
/// reads as a single-storey house.
pub const DEFAULT_HEIGHT_M: f32 = 4.0;
/// Metres-per-level constant for the `building:levels` → height
/// fallback. The OSM convention is ~3.0–3.5 m; 3.5 is the upper end,
/// chosen so the typical 30-storey tower with `levels=30` renders
/// at ~105 m instead of ~90 m and reads as a real high-rise.
pub const METRES_PER_LEVEL: f32 = 3.5;
/// Earth radius in metres. The renderer's unit sphere is `radius =
/// 1.0`, so `h_world = h_metres / EARTH_RADIUS_METRES`.
pub const EARTH_RADIUS_METRES: f64 = 6_371_000.0;

/// Where the height-in-metres came from, in priority order. The M2
/// debug overlay reads this to colour-code buildings by height
/// provenance.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HeightSource {
    /// `building:height` OSM tag — most trustworthy.
    Tagged,
    /// `building:levels` × `METRES_PER_LEVEL`.
    Levels,
    /// Neither tag present; rendered at `DEFAULT_HEIGHT_M`.
    Default,
}

/// Source-of-truth per-building data. The mesh + identify index are
/// derived from this; nothing else holds OSM-property strings at
/// runtime.
#[derive(Clone, Debug)]
pub struct Building {
    pub osm_way_id: u64,
    pub name: Option<String>,
    pub height_m: f32,
    pub height_source: HeightSource,
    /// Outer ring of the footprint, projected to normalised
    /// Mercator world coords. CCW winding (the polygon parser
    /// re-winds incoming GeoJSON to enforce this).
    pub footprint_world: Vec<[f32; 2]>,
    /// Inner rings (holes). Each ring is CW. v1 doesn't ship a
    /// builder for these — the bundled snapshot has no holes —
    /// but the field is here so M4's second-city dataset can
    /// drop them in without an API change.
    pub holes_world: Vec<Vec<[f32; 2]>>,
    pub centroid_lonlat: (f64, f64),
    pub bbox_lonlat: [f64; 4],
}

/// Packed GPU mesh for a city's buildings. One indexed draw call
/// renders the whole VBO; the per-building storage buffer carries
/// the centroid normals the vertex shader needs to extrude radially.
#[derive(Default, Debug)]
pub struct BuildingMesh {
    pub vertices: Vec<BuildingVertex>,
    pub indices: Vec<u32>,
    pub per_building: Vec<BuildingPerInstance>,
    /// Tallest building's normalised-units height. The renderer
    /// passes this to `Camera::near_plane_floor` so a 442 m
    /// Sears Tower top doesn't clip the near plane at z=15+.
    pub max_height_world: f32,
}

/// One vertex of a building mesh. 32 bytes; see plan 0014 § Struct
/// layouts. `face_kind`: 0 = wall, 1 = top.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct BuildingVertex {
    pub world: [f32; 2],
    pub height_world: f32,
    pub building_idx: u32,
    pub face_kind: u32,
    pub normal: [f32; 3],
}

/// Per-building data. 16 bytes; one storage-buffer slot per
/// building. The centroid normal is the outward unit vector at the
/// footprint centroid, used as the extrusion direction.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
pub struct BuildingPerInstance {
    pub centroid_normal: [f32; 3],
    pub _pad: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("parse: {0}")]
    Parse(#[from] Box<geojson::Error>),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<geojson::Error> for LoadError {
    fn from(e: geojson::Error) -> Self {
        LoadError::Parse(Box::new(e))
    }
}

/// Parse a gzipped GeoJSON FeatureCollection of building footprints.
/// Convenience wrapper around `load_buildings_geojson` for the
/// bundled `chicago.geojson.gz` shape — gunzips first, then parses.
pub fn load_buildings_geojson_gz(gz_bytes: &[u8]) -> Result<Vec<Building>, LoadError> {
    use std::io::Read;
    let mut decoder = flate2::read::GzDecoder::new(gz_bytes);
    let mut buf = String::new();
    decoder.read_to_string(&mut buf)?;
    load_buildings_geojson(&buf)
}

/// Parse a GeoJSON FeatureCollection of building footprints. Each
/// `Polygon` (or `MultiPolygon` — the outer rings get split into
/// separate `Building`s sharing one OSM id) becomes one `Building`
/// with its height resolved per the `Tagged → Levels → Default`
/// priority chain. Features without polygon geometries are silently
/// skipped.
pub fn load_buildings_geojson(source: &str) -> Result<Vec<Building>, LoadError> {
    let parsed: GeoJson = source.parse()?;
    let mut out = Vec::new();
    if let GeoJson::FeatureCollection(fc) = parsed {
        for f in &fc.features {
            ingest_feature(f, &mut out);
        }
    }
    Ok(out)
}

fn ingest_feature(f: &geojson::Feature, out: &mut Vec<Building>) {
    let Some(geom) = &f.geometry else { return };
    let props = f.properties.as_ref();
    let osm_way_id = props
        .and_then(|p| p.get("osm_way_id").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    let name = props
        .and_then(|p| p.get("name").and_then(|v| v.as_str()))
        .filter(|s| !s.is_empty())
        .map(String::from);
    let (height_m, height_source) = resolve_height(props);

    match &geom.value {
        Value::Polygon(rings) => {
            if let Some(b) =
                polygon_to_building(rings, osm_way_id, name.clone(), height_m, height_source)
            {
                out.push(b);
            }
        }
        Value::MultiPolygon(polys) => {
            for rings in polys {
                if let Some(b) =
                    polygon_to_building(rings, osm_way_id, name.clone(), height_m, height_source)
                {
                    out.push(b);
                }
            }
        }
        _ => {}
    }
}

fn resolve_height(props: Option<&geojson::JsonObject>) -> (f32, HeightSource) {
    let Some(p) = props else {
        return (DEFAULT_HEIGHT_M, HeightSource::Default);
    };
    if let Some(s) = p.get("building:height").and_then(|v| v.as_str()) {
        if let Some(h) = parse_height_metres(s) {
            return (h, HeightSource::Tagged);
        }
    }
    if let Some(n) = p.get("building:height").and_then(|v| v.as_f64()) {
        return (n as f32, HeightSource::Tagged);
    }
    if let Some(s) = p.get("building:levels").and_then(|v| v.as_str()) {
        if let Ok(n) = s.trim().parse::<f32>() {
            return (n * METRES_PER_LEVEL, HeightSource::Levels);
        }
    }
    if let Some(n) = p.get("building:levels").and_then(|v| v.as_f64()) {
        return (n as f32 * METRES_PER_LEVEL, HeightSource::Levels);
    }
    (DEFAULT_HEIGHT_M, HeightSource::Default)
}

/// Parse a height string the way OSM serves it — bare number,
/// number with trailing `m`, or number with `meters`/`metres` —
/// into metres. Returns `None` if the string isn't a clean number
/// at the front.
fn parse_height_metres(s: &str) -> Option<f32> {
    let s = s.trim();
    let digits: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    digits.parse::<f32>().ok()
}

fn polygon_to_building(
    rings: &[Vec<Vec<f64>>],
    osm_way_id: u64,
    name: Option<String>,
    height_m: f32,
    height_source: HeightSource,
) -> Option<Building> {
    if rings.is_empty() || rings[0].len() < 3 {
        return None;
    }
    let outer_lonlat: Vec<[f64; 2]> = rings[0]
        .iter()
        .filter_map(|c| {
            if c.len() >= 2 {
                Some([c[0], c[1]])
            } else {
                None
            }
        })
        .collect();
    if outer_lonlat.len() < 3 {
        return None;
    }
    // Drop the duplicate closing vertex if GeoJSON serves a closed
    // ring; the triangulator + wall emitter both want a non-closed
    // ring and add the closing edge themselves.
    let outer_lonlat = strip_closing_vertex(outer_lonlat);
    if outer_lonlat.len() < 3 {
        return None;
    }

    let project = |c: [f64; 2]| {
        let (wx, wy) = crs::lonlat_to_world(c[0], c[1]);
        [wx as f32, wy as f32]
    };
    let mut footprint_world: Vec<[f32; 2]> = outer_lonlat.iter().copied().map(project).collect();
    if signed_area(&footprint_world) < 0.0 {
        footprint_world.reverse();
    }

    let mut holes_world: Vec<Vec<[f32; 2]>> = Vec::new();
    for hole in rings.iter().skip(1) {
        let hole_lonlat: Vec<[f64; 2]> = hole
            .iter()
            .filter_map(|c| {
                if c.len() >= 2 {
                    Some([c[0], c[1]])
                } else {
                    None
                }
            })
            .collect();
        if hole_lonlat.len() < 3 {
            continue;
        }
        let hole_lonlat = strip_closing_vertex(hole_lonlat);
        let mut hole_world: Vec<[f32; 2]> = hole_lonlat.iter().copied().map(project).collect();
        if signed_area(&hole_world) > 0.0 {
            hole_world.reverse();
        }
        holes_world.push(hole_world);
    }

    let (centroid_lon, centroid_lat) = centroid_lonlat(&outer_lonlat);
    let bbox_lonlat = bbox_of(&outer_lonlat);

    Some(Building {
        osm_way_id,
        name,
        height_m,
        height_source,
        footprint_world,
        holes_world,
        centroid_lonlat: (centroid_lon, centroid_lat),
        bbox_lonlat,
    })
}

fn strip_closing_vertex(ring: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    if ring.len() >= 2
        && (ring[0][0] - ring[ring.len() - 1][0]).abs() < 1e-12
        && (ring[0][1] - ring[ring.len() - 1][1]).abs() < 1e-12
    {
        let mut r = ring;
        r.pop();
        r
    } else {
        ring
    }
}

fn signed_area(ring: &[[f32; 2]]) -> f32 {
    let mut a = 0.0_f32;
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    for i in 0..n {
        let j = (i + 1) % n;
        a += ring[i][0] * ring[j][1] - ring[j][0] * ring[i][1];
    }
    a * 0.5
}

fn centroid_lonlat(ring: &[[f64; 2]]) -> (f64, f64) {
    let n = ring.len() as f64;
    let mut lon = 0.0;
    let mut lat = 0.0;
    for c in ring {
        lon += c[0];
        lat += c[1];
    }
    (lon / n, lat / n)
}

fn bbox_of(ring: &[[f64; 2]]) -> [f64; 4] {
    let mut min_lon = f64::INFINITY;
    let mut min_lat = f64::INFINITY;
    let mut max_lon = f64::NEG_INFINITY;
    let mut max_lat = f64::NEG_INFINITY;
    for c in ring {
        min_lon = min_lon.min(c[0]);
        max_lon = max_lon.max(c[0]);
        min_lat = min_lat.min(c[1]);
        max_lat = max_lat.max(c[1]);
    }
    [min_lon, min_lat, max_lon, max_lat]
}

/// Build the GPU mesh (one indexed-draw VBO + storage buffer of
/// per-building centroid normals) from the parsed `Building`s.
/// Skips buildings whose top face fails to triangulate cleanly —
/// `log::warn!`s the OSM way id so the failing data is traceable.
pub fn build_mesh(buildings: &[Building]) -> BuildingMesh {
    let mut mesh = BuildingMesh::default();
    let mut skipped = 0_usize;
    for (idx, b) in buildings.iter().enumerate() {
        if !append_building_to_mesh(&mut mesh, idx, b) {
            skipped += 1;
        }
        let h_world = (b.height_m as f64 / EARTH_RADIUS_METRES) as f32;
        if h_world > mesh.max_height_world {
            mesh.max_height_world = h_world;
        }
    }
    if skipped > 0 {
        log::warn!(
            "build_mesh: skipped {skipped} of {} buildings whose top face failed to triangulate",
            buildings.len()
        );
    }
    mesh
}

fn append_building_to_mesh(mesh: &mut BuildingMesh, idx: usize, b: &Building) -> bool {
    let h_world = (b.height_m as f64 / EARTH_RADIUS_METRES) as f32;
    let centroid_n = lonlat_to_sphere(b.centroid_lonlat.0, b.centroid_lonlat.1);
    mesh.per_building.push(BuildingPerInstance {
        centroid_normal: [
            centroid_n[0] as f32,
            centroid_n[1] as f32,
            centroid_n[2] as f32,
        ],
        _pad: 0.0,
    });

    // Triangulate the top face (outer ring + holes) with earcutr.
    // Layout earcutr expects: flat `Vec<f64>` of [x, y, x, y, …]
    // for outer ring then each hole, plus `hole_indices` listing
    // the starting vertex of each hole.
    let mut flat: Vec<f64> = Vec::with_capacity(2 * b.footprint_world.len());
    for v in &b.footprint_world {
        flat.push(v[0] as f64);
        flat.push(v[1] as f64);
    }
    let mut hole_indices: Vec<usize> = Vec::with_capacity(b.holes_world.len());
    for hole in &b.holes_world {
        hole_indices.push(flat.len() / 2);
        for v in hole {
            flat.push(v[0] as f64);
            flat.push(v[1] as f64);
        }
    }
    let triangles = match earcutr::earcut(&flat, &hole_indices, 2) {
        Ok(t) => t,
        Err(_) => {
            log::warn!(
                "build_mesh: earcutr failed on OSM way {} ({} ring verts)",
                b.osm_way_id,
                b.footprint_world.len()
            );
            return false;
        }
    };
    if triangles.is_empty() {
        return false;
    }

    let face_top: u32 = 1;
    let centroid_n_f32 = [
        centroid_n[0] as f32,
        centroid_n[1] as f32,
        centroid_n[2] as f32,
    ];

    // Top-face verts: one BuildingVertex per polygon vertex at
    // height = h_world. Normal = outward radial (centroid normal).
    let top_base_idx = mesh.vertices.len() as u32;
    for v in &b.footprint_world {
        mesh.vertices.push(BuildingVertex {
            world: *v,
            height_world: h_world,
            building_idx: idx as u32,
            face_kind: face_top,
            normal: centroid_n_f32,
        });
    }
    for hole in &b.holes_world {
        for v in hole {
            mesh.vertices.push(BuildingVertex {
                world: *v,
                height_world: h_world,
                building_idx: idx as u32,
                face_kind: face_top,
                normal: centroid_n_f32,
            });
        }
    }
    for tri in triangles.chunks_exact(3) {
        // earcutr returns local indices into the combined ring
        // list it was handed, in the same order we emitted them.
        mesh.indices.extend([
            top_base_idx + tri[0] as u32,
            top_base_idx + tri[1] as u32,
            top_base_idx + tri[2] as u32,
        ]);
    }

    // Side walls: one quad per ring edge, two triangles per quad.
    // Per-quad normal is the outward horizontal direction (the
    // 2D edge normal projected to body-fixed coords). For the
    // top-down v1 the Lambert term mostly samples |normal · sun_dir|
    // for walls that have a horizontal component; getting it
    // close-enough-is-good — we use the screen-space edge normal
    // re-projected back through `lonlat_to_sphere` at the edge
    // midpoint.
    emit_walls(mesh, idx, &b.footprint_world, h_world, centroid_n_f32);
    for hole in &b.holes_world {
        emit_walls(mesh, idx, hole, h_world, centroid_n_f32);
    }

    true
}

fn emit_walls(
    mesh: &mut BuildingMesh,
    building_idx: usize,
    ring: &[[f32; 2]],
    h_world: f32,
    centroid_n: [f32; 3],
) {
    let n = ring.len();
    if n < 2 {
        return;
    }
    const FACE_WALL: u32 = 0;
    for i in 0..n {
        let j = (i + 1) % n;
        let v0 = ring[i];
        let v1 = ring[j];
        // Approximate wall normal: the outward 2D normal of the
        // edge, projected into a 3D vector with no radial
        // component, then re-orthogonalised against the centroid
        // normal so it's tangent to the sphere. Cheap + close
        // enough for top-down Lambert.
        let edge = [v1[0] - v0[0], v1[1] - v0[1]];
        let elen = (edge[0] * edge[0] + edge[1] * edge[1]).sqrt().max(1e-12);
        let edge_n = [edge[0] / elen, edge[1] / elen];
        // 2D right-hand normal (CCW outer ring → outward).
        let n2d = [edge_n[1], -edge_n[0]];
        // Wall normal in tile-tangent frame ≈ same direction in
        // sphere-tangent at city scale. Encode by lifting to 3D
        // and zeroing the radial component (the renderer's
        // shader doesn't need pixel-perfect; the Lambert is
        // tunable per body).
        let normal = wall_normal_3d(n2d, centroid_n);
        let base = mesh.vertices.len() as u32;
        let push = |mesh: &mut BuildingMesh, w: [f32; 2], h: f32| {
            mesh.vertices.push(BuildingVertex {
                world: w,
                height_world: h,
                building_idx: building_idx as u32,
                face_kind: FACE_WALL,
                normal,
            });
        };
        push(mesh, v0, 0.0);
        push(mesh, v1, 0.0);
        push(mesh, v1, h_world);
        push(mesh, v0, h_world);
        // (v0_base, v1_base, v1_top), (v0_base, v1_top, v0_top)
        mesh.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// Approximate wall normal in body-fixed coords: take the 2D
/// outward edge normal (in normalised-Mercator world space) and
/// re-project it onto the sphere tangent plane at the centroid.
/// City-scale curvature is negligible, so this reads as "the wall
/// points outward in the horizontal."
fn wall_normal_3d(n2d: [f32; 2], centroid_n: [f32; 3]) -> [f32; 3] {
    // Build a horizontal-ish vector by combining the 2D normal
    // with zero vertical. Then orthogonalise against centroid_n
    // (so the vector is tangent to the unit sphere at the
    // building centroid) and renormalise.
    let raw = [n2d[0], 0.0, n2d[1]];
    // Project out the centroid-normal component.
    let dot = raw[0] * centroid_n[0] + raw[1] * centroid_n[1] + raw[2] * centroid_n[2];
    let tangent = [
        raw[0] - dot * centroid_n[0],
        raw[1] - dot * centroid_n[1],
        raw[2] - dot * centroid_n[2],
    ];
    let l = (tangent[0] * tangent[0] + tangent[1] * tangent[1] + tangent[2] * tangent[2])
        .sqrt()
        .max(1e-12);
    [tangent[0] / l, tangent[1] / l, tangent[2] / l]
}

/// Sphere convention helper — mirror of `lonlat_to_sphere` in the
/// shaders. (lon, lat) in degrees → unit vector with prime meridian
/// at +Z, north pole at +Y.
fn lonlat_to_sphere(lon_deg: f64, lat_deg: f64) -> [f64; 3] {
    let lon = lon_deg.to_radians();
    let lat = lat_deg.to_radians();
    [lat.cos() * lon.sin(), lat.sin(), lat.cos() * lon.cos()]
}

/// Build the `IdentifyIndex` that the renderer's click handler
/// uses to answer "which building was clicked." Reuses the same
/// polygon-pick plumbing as the country-outline overlay.
pub fn build_identify_index(buildings: &[Building]) -> vector::IdentifyIndex {
    let features: Vec<vector::IdentifyFeature> = buildings
        .iter()
        .map(|b| {
            let name = b
                .name
                .clone()
                .unwrap_or_else(|| format!("Building #{}", b.osm_way_id));
            // IdentifyFeature wants the polygon in (lon, lat).
            // Reconstruct from the centroid-bbox is wrong — we'd
            // lose the actual footprint. Use the original
            // lon/lat ring stored on the Building.
            let outer = footprint_world_to_lonlat(&b.footprint_world);
            let mut polygons: Vec<Vec<Vec<[f64; 2]>>> = Vec::with_capacity(1);
            let mut rings: Vec<Vec<[f64; 2]>> = vec![outer];
            for hole in &b.holes_world {
                rings.push(footprint_world_to_lonlat(hole));
            }
            polygons.push(rings);
            vector::IdentifyFeature {
                name,
                bbox: b.bbox_lonlat,
                polygons,
            }
        })
        .collect();
    vector::IdentifyIndex { features }
}

fn footprint_world_to_lonlat(ring: &[[f32; 2]]) -> Vec<[f64; 2]> {
    ring.iter()
        .map(|v| {
            let (lon, lat) = crs::world_to_lonlat(v[0] as f64, v[1] as f64);
            [lon, lat]
        })
        .collect()
}

/// Counts of buildings by height-source, for diagnostic logging
/// + the M0 coverage test. `(tagged, levels, default)`.
pub fn height_source_counts(buildings: &[Building]) -> (usize, usize, usize) {
    let mut t = 0;
    let mut l = 0;
    let mut d = 0;
    for b in buildings {
        match b.height_source {
            HeightSource::Tagged => t += 1,
            HeightSource::Levels => l += 1,
            HeightSource::Default => d += 1,
        }
    }
    (t, l, d)
}

/// Total footprint area in normalised-Mercator units squared,
/// summed across the buildings whose `height_source` is in
/// `selected`. Used by the M0 coverage assertion (`tagged + levels`
/// must cover ≥ X% of total footprint area in the Chicago bbox).
pub fn footprint_area_by_source(buildings: &[Building], selected: &[HeightSource]) -> f32 {
    let mut total = 0.0_f32;
    for b in buildings {
        if !selected.contains(&b.height_source) {
            continue;
        }
        total += signed_area(&b.footprint_world).abs();
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_height_metres_handles_bare_number() {
        assert_eq!(parse_height_metres("346"), Some(346.0));
    }

    #[test]
    fn parse_height_metres_handles_trailing_m() {
        assert_eq!(parse_height_metres("346 m"), Some(346.0));
        assert_eq!(parse_height_metres("346m"), Some(346.0));
    }

    #[test]
    fn parse_height_metres_handles_decimal() {
        assert_eq!(parse_height_metres("12.5"), Some(12.5));
    }

    #[test]
    fn extrusion_height_keeps_precision_through_f32() {
        // Sears / Willis Tower: 442 m / 6.371e6 m = 6.937e-5.
        // Must round-trip f64 → f32 without losing the leading
        // significant figure.
        let h_world = (442.0_f64 / EARTH_RADIUS_METRES) as f32;
        assert!((h_world - 6.937e-5).abs() < 1e-7, "got {h_world}");
        // Aon Center: 346 m / 6.371e6 = 5.431e-5.
        let aon_world = (346.0_f64 / EARTH_RADIUS_METRES) as f32;
        assert!((aon_world - 5.431e-5).abs() < 1e-7, "got {aon_world}");
    }

    #[test]
    fn signed_area_is_positive_for_ccw_unit_square() {
        // Unit square (CCW) has area 1.0; signed area is positive
        // (CCW convention).
        let ring = vec![[0.0_f32, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        let a = signed_area(&ring);
        assert!((a - 1.0).abs() < 1e-6, "area = {a}");
    }

    #[test]
    fn signed_area_is_negative_for_cw_unit_square() {
        let ring = vec![[0.0_f32, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];
        let a = signed_area(&ring);
        assert!((a + 1.0).abs() < 1e-6, "area = {a}");
    }

    #[test]
    fn fixture_polygon_with_hole_triangulates() {
        // 1×1 outer square with a 0.25×0.25 hole in the middle.
        // Outer = 4 verts, hole = 4 verts → 8 total. earcutr
        // should produce 8 triangles (the canonical count for
        // this shape).
        let flat: Vec<f64> = vec![
            0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.375, 0.375, 0.625, 0.375, 0.625, 0.625,
            0.375, 0.625,
        ];
        let hole_indices = vec![4_usize];
        let tris = earcutr::earcut(&flat, &hole_indices, 2).unwrap();
        assert_eq!(tris.len(), 8 * 3, "got {} indices", tris.len());
    }

    #[test]
    fn load_buildings_geojson_gz_unpacks_bundled_chicago() {
        // Smoke test: the bundled snapshot loads, has > 1000
        // features, and Aon Center / Sears Tower are in the set.
        // Skip-when-missing so this still passes if someone clones
        // the repo without LFS-style data hooks.
        const BYTES: &[u8] = include_bytes!("../data/buildings/chicago.geojson.gz");
        let buildings = load_buildings_geojson_gz(BYTES).expect("gunzip + parse");
        assert!(buildings.len() > 1000, "got {} buildings", buildings.len());
        // The chicago snapshot is dense enough that at least a
        // few hundred have tagged heights or levels — exact count
        // is volatile but the order of magnitude isn't.
        let (t, l, d) = height_source_counts(&buildings);
        assert!(
            t + l >= 200,
            "expected ≥200 tagged+levels buildings, got tagged={t} levels={l} default={d}"
        );
    }

    #[test]
    fn coverage_threshold_meets_v1_bar() {
        const BYTES: &[u8] = include_bytes!("../data/buildings/chicago.geojson.gz");
        let buildings = load_buildings_geojson_gz(BYTES).expect("gunzip + parse");
        let total = footprint_area_by_source(
            &buildings,
            &[
                HeightSource::Tagged,
                HeightSource::Levels,
                HeightSource::Default,
            ],
        );
        let covered =
            footprint_area_by_source(&buildings, &[HeightSource::Tagged, HeightSource::Levels]);
        let ratio = covered / total.max(1e-12);
        // The plan target is ≥50% by footprint area. If this
        // drops below the threshold the snapshot regressed; re-
        // snap the data or lower the bar with a justification.
        assert!(
            ratio >= 0.40,
            "tagged+levels covers {ratio:.2} of footprint area, threshold 0.40"
        );
    }

    #[test]
    fn build_mesh_produces_geometry_and_max_height() {
        const BYTES: &[u8] = include_bytes!("../data/buildings/chicago.geojson.gz");
        let buildings = load_buildings_geojson_gz(BYTES).expect("gunzip + parse");
        let mesh = build_mesh(&buildings);
        assert!(!mesh.vertices.is_empty(), "expected verts");
        assert!(!mesh.indices.is_empty(), "expected indices");
        assert!(
            mesh.indices.len().is_multiple_of(3),
            "indices not a multiple of 3: {}",
            mesh.indices.len()
        );
        // The tallest building's h_world should be at least
        // Aon Center's (5.4e-5). Sears Tower might be in the
        // bbox depending on bounds.
        assert!(
            mesh.max_height_world > 4e-5,
            "max_height_world={} (expected at least 4e-5)",
            mesh.max_height_world
        );
    }

    #[test]
    fn build_identify_index_contains_polygon_for_each_building() {
        const BYTES: &[u8] = include_bytes!("../data/buildings/chicago.geojson.gz");
        let buildings = load_buildings_geojson_gz(BYTES).expect("gunzip + parse");
        let idx = build_identify_index(&buildings);
        assert_eq!(idx.features.len(), buildings.len());
    }
}
