# Mars fallback texture

## File

`mars_2048x1024.jpg` — equirectangular Plate Carrée projection of
Mars, 2048×1024 px, ~516 KB. Used as the under-tile fallback the
Mars globe shows before NASA Trek tiles stream in.

## Source

Built by stitching the z=2 Mars Viking Color Mosaic tile pyramid
from NASA's Mars Trek WMTS:

```
https://trek.nasa.gov/tiles/Mars/EQ/Mars_Viking_MDIM21_ClrMosaic_global_232m/1.0.0/default/default028mm/{z}/{y}/{x}.jpg
```

z=2 gives an 8×4 tile grid (the NASA Trek EQ convention is
`2·2^z` wide × `2^z` tall), each tile 256×256 — that produces a
2048×1024 stitched mosaic at ~232 m/px nominal resolution
(downscaled from the Viking MDIM 2.1 mosaic). The stitch script
lives in this commit's body for reproducibility.

## License

Mars Viking MDIM 2.1 is a **U.S. Government work, public domain
worldwide**. NASA's [media usage
guidelines](https://www.nasa.gov/multimedia/guidelines/index.html)
ask for credit; aeGIS's attribution footer surfaces "Imagery: NASA
/ Viking MDIM 2.1" when the Mars body is active.
