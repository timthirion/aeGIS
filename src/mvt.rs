//! Minimal MVT (Mapbox Vector Tile) decoder for building footprints.
//!
//! Decodes the `building` layer from an OpenFreeMap tile into
//! [`Building`](crate::buildings::Building) structs the existing
//! [`build_mesh`](crate::buildings::build_mesh) pipeline can consume.
//! No external protobuf crate needed — MVT tiles from OFM use only
//! varint and length-delimited fields, which this module handles
//! directly.
//!
//! OFM property names (different from raw OSM tags):
//! - `render_height`: pre-computed building height in metres
//! - `render_min_height`: base height for split-level buildings (unused v1)

use crate::{
    buildings::{Building, HeightSource, DEFAULT_HEIGHT_M},
    crs,
    tile::TileId,
};

#[derive(Debug, thiserror::Error)]
pub enum MvtError {
    #[error("truncated tile")]
    Truncated,
}

/// Decode a raw MVT tile body into [`Building`] structs. Only processes
/// the `"building"` layer; all other layers are skipped without
/// allocation. The `tile_id` is needed to inverse-project MVT pixel
/// coords (0..extent) to Web Mercator world coords ([0,1]).
pub fn decode_mvt_buildings(bytes: &[u8], tile_id: TileId) -> Result<Vec<Building>, MvtError> {
    let mut out = Vec::new();
    let mut r = Reader::new(bytes);
    while !r.done() {
        let Some((field, wire)) = r.tag() else {
            break;
        };
        if field == 3 && wire == 2 {
            let layer_bytes = r.ld().ok_or(MvtError::Truncated)?;
            decode_layer(layer_bytes, tile_id, &mut out)?;
        } else {
            r.skip(wire);
        }
    }
    Ok(out)
}

fn decode_layer<'a>(
    bytes: &'a [u8],
    tile_id: TileId,
    out: &mut Vec<Building>,
) -> Result<(), MvtError> {
    let mut name: &'a str = "";
    let mut keys: Vec<&'a str> = Vec::new();
    let mut values: Vec<&'a [u8]> = Vec::new();
    let mut features: Vec<&'a [u8]> = Vec::new();
    let mut extent = 4096u32;

    let mut r = Reader::new(bytes);
    while !r.done() {
        let Some((field, wire)) = r.tag() else {
            break;
        };
        match field {
            1 => {
                let s = r.ld().ok_or(MvtError::Truncated)?;
                name = std::str::from_utf8(s).unwrap_or("");
            }
            2 => {
                let s = r.ld().ok_or(MvtError::Truncated)?;
                features.push(s);
            }
            3 => {
                let s = r.ld().ok_or(MvtError::Truncated)?;
                keys.push(std::str::from_utf8(s).unwrap_or(""));
            }
            4 => {
                let s = r.ld().ok_or(MvtError::Truncated)?;
                values.push(s);
            }
            5 => {
                extent = r.varint().ok_or(MvtError::Truncated)? as u32;
            }
            _ => {
                r.skip(wire);
            }
        }
    }

    if name != "building" {
        return Ok(());
    }

    let height_ki = keys.iter().position(|&k| k == "render_height");
    let extent_f = extent as f32;
    let n = (1u32 << tile_id.z) as f32;

    for feat_bytes in &features {
        decode_feature(feat_bytes, tile_id, extent_f, n, height_ki, &values, out);
    }
    Ok(())
}

fn decode_feature(
    bytes: &[u8],
    tile_id: TileId,
    extent_f: f32,
    n: f32,
    height_ki: Option<usize>,
    values: &[&[u8]],
    out: &mut Vec<Building>,
) {
    let mut id = 0u64;
    let mut tags_packed: &[u8] = &[];
    let mut geom_type = 0u32;
    let mut geom_packed: &[u8] = &[];

    let mut r = Reader::new(bytes);
    while !r.done() {
        let Some((field, _wire)) = r.tag() else {
            break;
        };
        match field {
            1 => {
                id = r.varint().unwrap_or(0);
            }
            2 => {
                tags_packed = r.ld().unwrap_or(&[]);
            }
            3 => {
                geom_type = r.varint().unwrap_or(0) as u32;
            }
            4 => {
                geom_packed = r.ld().unwrap_or(&[]);
            }
            _ => {
                r.skip(_wire);
            }
        }
    }

    if geom_type != 3 {
        return; // only polygons
    }

    let geom = packed_u32(geom_packed);
    let rings_px = geometry_rings(&geom);
    if rings_px.is_empty() || rings_px[0].len() < 3 {
        return;
    }

    // Height from OFM `render_height` tag (already in metres)
    let mut height_m = DEFAULT_HEIGHT_M;
    if let Some(h_ki) = height_ki {
        let tags = packed_u32(tags_packed);
        for pair in tags.chunks_exact(2) {
            if pair[0] as usize == h_ki {
                if let Some(vb) = values.get(pair[1] as usize) {
                    if let Some(v) = value_as_f64(vb) {
                        if v > 0.0 {
                            height_m = v as f32;
                        }
                    }
                }
            }
        }
    }

    // Convert outer ring from MVT pixel coords → normalised Web Mercator [0,1]
    let project = |[px, py]: [f32; 2]| -> [f32; 2] {
        [
            (tile_id.x as f32 + px / extent_f) / n,
            (tile_id.y as f32 + py / extent_f) / n,
        ]
    };

    let mut outer: Vec<[f32; 2]> = rings_px[0].iter().copied().map(project).collect();
    if outer.len() < 3 {
        return;
    }
    // Ensure CCW winding (positive signed area) for earcutr
    if signed_area(&outer) < 0.0 {
        outer.reverse();
    }

    let holes: Vec<Vec<[f32; 2]>> = rings_px[1..]
        .iter()
        .map(|ring| {
            let mut hw: Vec<[f32; 2]> = ring.iter().copied().map(project).collect();
            // Holes must be CW (negative signed area)
            if signed_area(&hw) > 0.0 {
                hw.reverse();
            }
            hw
        })
        .collect();

    // Centroid and bbox in degrees (build_mesh converts to sphere via lonlat_to_sphere)
    let cx = outer.iter().map(|v| v[0] as f64).sum::<f64>() / outer.len() as f64;
    let cy = outer.iter().map(|v| v[1] as f64).sum::<f64>() / outer.len() as f64;
    let (clon, clat) = crs::world_to_lonlat(cx, cy);

    let min_x = outer
        .iter()
        .map(|v| v[0] as f64)
        .fold(f64::INFINITY, f64::min);
    let min_y = outer
        .iter()
        .map(|v| v[1] as f64)
        .fold(f64::INFINITY, f64::min);
    let max_x = outer
        .iter()
        .map(|v| v[0] as f64)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_y = outer
        .iter()
        .map(|v| v[1] as f64)
        .fold(f64::NEG_INFINITY, f64::max);
    // Web Mercator y increases southward: min_y = northernmost → max_lat
    let (min_lon, max_lat) = crs::world_to_lonlat(min_x, min_y);
    let (max_lon, min_lat) = crs::world_to_lonlat(max_x, max_y);

    out.push(Building {
        osm_way_id: id,
        name: None, // OFM building layer doesn't expose name
        height_m,
        height_source: HeightSource::Tagged,
        footprint_world: outer,
        holes_world: holes,
        centroid_lonlat: (clon, clat),
        bbox_lonlat: [min_lon, min_lat, max_lon, max_lat],
    });
}

/// Parse geometry command stream into rings of MVT pixel coords.
/// Ring 0 is the exterior; rings 1+ are holes.
fn geometry_rings(geom: &[u32]) -> Vec<Vec<[f32; 2]>> {
    let mut rings: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut ring: Vec<[f32; 2]> = Vec::new();
    let mut cx = 0i32;
    let mut cy = 0i32;
    let mut i = 0;

    while i < geom.len() {
        let cmd_int = geom[i];
        let cmd = cmd_int & 0x7;
        let count = (cmd_int >> 3) as usize;
        i += 1;

        match cmd {
            1 => {
                // MoveTo — save previous ring (if large enough) and start new
                if ring.len() >= 3 {
                    rings.push(std::mem::take(&mut ring));
                } else {
                    ring.clear();
                }
                for _ in 0..count {
                    if i + 1 >= geom.len() {
                        return rings;
                    }
                    cx += zigzag(geom[i]);
                    cy += zigzag(geom[i + 1]);
                    i += 2;
                    ring.push([cx as f32, cy as f32]);
                }
            }
            2 => {
                // LineTo
                for _ in 0..count {
                    if i + 1 >= geom.len() {
                        return rings;
                    }
                    cx += zigzag(geom[i]);
                    cy += zigzag(geom[i + 1]);
                    i += 2;
                    ring.push([cx as f32, cy as f32]);
                }
            }
            7 => {
                // ClosePath — no parameters; save completed ring
                if ring.len() >= 3 {
                    rings.push(std::mem::take(&mut ring));
                } else {
                    ring.clear();
                }
            }
            _ => break,
        }
    }

    if ring.len() >= 3 {
        rings.push(ring);
    }
    rings
}

fn zigzag(n: u32) -> i32 {
    ((n >> 1) as i32) ^ -((n & 1) as i32)
}

fn packed_u32(bytes: &[u8]) -> Vec<u32> {
    let mut r = Reader::new(bytes);
    let mut v = Vec::new();
    while !r.done() {
        match r.varint() {
            Some(n) => v.push(n as u32),
            None => break,
        }
    }
    v
}

/// Extract a numeric value from a serialised MVT `Value` message.
fn value_as_f64(bytes: &[u8]) -> Option<f64> {
    let mut r = Reader::new(bytes);
    while !r.done() {
        let (field, wire) = r.tag()?;
        match (field, wire) {
            (2, 5) => {
                // float_value
                if r.pos + 4 > r.buf.len() {
                    return None;
                }
                let b = [
                    r.buf[r.pos],
                    r.buf[r.pos + 1],
                    r.buf[r.pos + 2],
                    r.buf[r.pos + 3],
                ];
                return Some(f32::from_le_bytes(b) as f64);
            }
            (3, 1) => {
                // double_value
                if r.pos + 8 > r.buf.len() {
                    return None;
                }
                let b: [u8; 8] = r.buf[r.pos..r.pos + 8].try_into().ok()?;
                return Some(f64::from_le_bytes(b));
            }
            (4, 0) => {
                // int_value (varint, interpret as signed via two's complement)
                let v = r.varint()?;
                return Some(v as i64 as f64);
            }
            (5, 0) => {
                // uint_value
                let v = r.varint()?;
                return Some(v as f64);
            }
            (6, 0) => {
                // sint_value (zigzag)
                let v = r.varint()?;
                return Some((((v >> 1) as i64) ^ -((v & 1) as i64)) as f64);
            }
            _ => {
                r.skip(wire);
            }
        }
    }
    None
}

fn signed_area(ring: &[[f32; 2]]) -> f32 {
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    let mut a = 0.0_f32;
    for i in 0..n {
        let j = (i + 1) % n;
        a += ring[i][0] * ring[j][1] - ring[j][0] * ring[i][1];
    }
    a * 0.5
}

/// Minimal protobuf reader: varint, length-delimited, skip.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn varint(&mut self) -> Option<u64> {
        let mut result = 0u64;
        let mut shift = 0u32;
        loop {
            if self.pos >= self.buf.len() {
                return None;
            }
            let b = self.buf[self.pos];
            self.pos += 1;
            result |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
            if shift >= 64 {
                return None;
            }
        }
    }

    fn tag(&mut self) -> Option<(u32, u8)> {
        let v = self.varint()? as u32;
        Some((v >> 3, (v & 0x7) as u8))
    }

    fn ld(&mut self) -> Option<&'a [u8]> {
        let len = self.varint()? as usize;
        if self.pos + len > self.buf.len() {
            return None;
        }
        let s = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        Some(s)
    }

    fn skip(&mut self, wire: u8) {
        match wire {
            0 => {
                self.varint();
            }
            1 => {
                self.pos = (self.pos + 8).min(self.buf.len());
            }
            2 => {
                self.ld();
            }
            5 => {
                self.pos = (self.pos + 4).min(self.buf.len());
            }
            _ => {
                self.pos = self.buf.len(); // unknown wire type — abort parse
            }
        }
    }
}
