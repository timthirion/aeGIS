# Natural Earth data

This directory bundles vector data from
[Natural Earth](https://www.naturalearthdata.com/) — a public-domain
map dataset maintained by the North American Cartographic Information
Society (NACIS).

## License

> All Natural Earth data is in the public domain. You are free to
> use the data in any manner, including modifying the content and
> design, electronic dissemination, and offset printing. The
> primary authors, Tom Patterson and Nathaniel Vaughn Kelso, and
> all other contributors renounce all financial claim to the data
> and invite you to use them for personal, educational, and
> commercial purposes.

Source: <https://www.naturalearthdata.com/about/terms-of-use/>.

No attribution is required, but is appreciated.

## Files

- `countries.geojson` — `ne_110m_admin_0_countries` (1:110 million scale).
  Country polygons covering the world. Suitable for low-zoom overviews
  (zoom ≤ 5); higher zoom levels want `1:50m` or `1:10m`.

  Sourced from the [martynafford/natural-earth-geojson](https://github.com/martynafford/natural-earth-geojson)
  mirror (which republishes the Natural Earth shapefiles as GeoJSON
  with no semantic changes).

## Refresh

To re-fetch the source file:

```sh
curl -sL -o data/natural-earth/countries.geojson \
  https://raw.githubusercontent.com/martynafford/natural-earth-geojson/master/110m/cultural/ne_110m_admin_0_countries.json
```
