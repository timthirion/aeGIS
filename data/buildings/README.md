# Building footprints (plan 0014)

`chicago.geojson.gz` is a GeoJSON FeatureCollection of OpenStreetMap building
footprints + height tags for downtown Chicago. Polygon geometries; properties
trimmed to `osm_way_id`, `building`, `building:levels`, `building:height`,
`name`. **7 911 features, ~516 KB gzipped** (the larger-than-anticipated size
vs the 80 KB plan estimate reflects denser-than-expected building coverage in
the chosen bbox — the Loop + River North + part of the West Loop have very
high OSM building completeness).

## Source

- **OpenStreetMap**, queried via the Overpass API at
  <https://overpass-api.de/api/interpreter>.
- **Bbox:** `41.87, -87.66, 41.91, -87.61` — covers the Loop, River North,
  Streeterville, and the eastern edge of the West Loop. Roughly 4.4 km × 4.2 km.
- **Query:**

  ```overpass
  [out:json][timeout:60];
  way["building"](41.87,-87.66,41.91,-87.61);
  out geom;
  ```

- **Snapshot date:** 2026-06-18.
- **License:** [Open Data Commons Open Database License (ODbL)](https://opendatacommons.org/licenses/odbl/) —
  same as OpenStreetMap upstream. Attribution `© OpenStreetMap contributors`
  is required for downstream use, and the live attribution panel in
  `index.html` carries the credit + the Overpass URL + this snapshot date
  while buildings are visible.

## Reproducing the snapshot

```sh
curl -sS -o _overpass.json --data-urlencode \
    'data=[out:json][timeout:60];way["building"](41.87,-87.66,41.91,-87.61);out geom;' \
    https://overpass-api.de/api/interpreter
python3 build.py _overpass.json chicago.geojson
gzip -9 -c chicago.geojson > chicago.geojson.gz
rm _overpass.json chicago.geojson  # only the .gz is checked in
```

`build.py` (alongside this README) converts Overpass JSON to GeoJSON while
stripping properties to the four the renderer reads. Re-snap whenever OSM
data drifts noticeably from the live render.

## Why not other sources

See `plans/0014-3d-buildings.md` § Data source for the trade-off discussion
(Overture, Microsoft Footprints, Protomaps vector tiles, USGS 3DEP all
considered + rejected for v1).
