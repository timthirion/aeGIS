#!/usr/bin/env python3
"""Convert an Overpass-API JSON dump of building ways into a trimmed
GeoJSON FeatureCollection that the renderer can consume.

Pulls every closed `way["building"]` from the input, strips properties
to the four that matter for the extrusion (`building`, `building:levels`,
`building:height`, `name`), and emits a single Polygon Feature per
building. Holes / multipolygons are not yet handled — the bbox is
small enough that the building-with-courtyard case (where the
courtyard is modelled as an OSM relation rather than as a polygon
with a hole) is rare; if it shows up we extend this script + the
loader together.

Usage:
    curl -sS -o _overpass.json --data-urlencode \\
        'data=[out:json][timeout:60];way["building"](41.87,-87.66,41.91,-87.61);out geom;' \\
        https://overpass-api.de/api/interpreter
    python3 build.py _overpass.json chicago.geojson
    gzip -9 -c chicago.geojson > chicago.geojson.gz
    rm _overpass.json chicago.geojson  # gzip is the checked-in artifact
"""

import json
import sys

KEEP_TAGS = ("building", "building:levels", "building:height", "name")


def overpass_way_to_feature(way):
    geom = way.get("geometry") or []
    if len(geom) < 4:
        return None
    coords = [[float(p["lon"]), float(p["lat"])] for p in geom]
    # Polygon outer ring must be closed.
    if coords[0] != coords[-1]:
        coords.append(coords[0])
    tags = way.get("tags") or {}
    props = {"osm_way_id": way["id"]}
    for k in KEEP_TAGS:
        if k in tags and tags[k]:
            props[k] = tags[k]
    return {
        "type": "Feature",
        "geometry": {"type": "Polygon", "coordinates": [coords]},
        "properties": props,
    }


def main(argv):
    if len(argv) != 3:
        sys.stderr.write(__doc__)
        sys.exit(2)
    with open(argv[1]) as f:
        op = json.load(f)
    features = []
    for el in op.get("elements", []):
        if el.get("type") != "way":
            continue
        f = overpass_way_to_feature(el)
        if f is not None:
            features.append(f)
    fc = {"type": "FeatureCollection", "features": features}
    with open(argv[2], "w") as out:
        json.dump(fc, out, separators=(",", ":"))
    sys.stderr.write(f"wrote {len(features)} features\n")


if __name__ == "__main__":
    main(sys.argv)
