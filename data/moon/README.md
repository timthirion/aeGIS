# Moon fallback texture

## File

`moon_2048x1024.jpg` — equirectangular Plate Carrée projection of
the Moon, 2048×1024 px, ~697 KB. Used as the under-tile fallback
the Moon globe shows before NASA Trek tiles stream in.

## Source

Built by stitching the z=2 LRO LROC WAC global mosaic tile
pyramid from NASA's Moon Trek WMTS:

```
https://trek.nasa.gov/tiles/Moon/EQ/LRO_WAC_Mosaic_Global_303ppd_v02/1.0.0/default/default028mm/{z}/{y}/{x}.jpg
```

Same 8×4 z=2 grid shape as the Mars texture, producing a 2048×1024
stitched mosaic at ~303 ppd (≈ 118 m/px equatorial).

## License

The LRO LROC WAC global mosaic is a **U.S. Government work,
public domain worldwide**. aeGIS's attribution footer surfaces
"Imagery: NASA / LRO LROC WAC" when the Moon body is active.
