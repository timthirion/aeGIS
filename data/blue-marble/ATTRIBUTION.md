# Blue Marble Earth imagery

`earth_4096x2048.jpg` is a 4096×2048 downsample of NASA's
**Blue Marble: Land Surface, Shallow Water, and Shaded Topography**
(`land_shallow_topo_8192.tif`), an open dataset by NASA / Goddard
Space Flight Center / Reto Stöckli.

## Provenance

- Source URL: <https://eoimages.gsfc.nasa.gov/images/imagerecords/57000/57752/land_shallow_topo_8192.tif>
- NASA's catalog entry: <https://visibleearth.nasa.gov/images/57752/blue-marble-land-surface-shallow-water-and-shaded-topography>
- Original: 8192 × 4096 TIFF (28 MB).
- Bundled: 4096 × 2048 JPEG (~ 1.6 MB), `sips` Lanczos downsample
  + quality-88 JPEG re-encode.

The previous bundled 2048×1024 (≈ 240 KB) read as blurry at globe
view because the visible hemisphere covers ~1024×1024 of the texture
mapped onto a sphere typically ≥ 1024 device-pixels across on
retina screens — the texture was effectively 1:1 with no oversampling
headroom. 4096×2048 gives 4× the linear density (16× more pixels),
restoring crispness across the full zoom range the satellite layer
covers.

WebGPU's downlevel WebGL2 default caps `max_texture_dimension_2d` at
2048; we explicitly raise it to 4096 in `request_device` to fit this
texture. 4096 is at-or-below the limit on virtually every modern
GPU including mobile WebGPU. Larger sources (the original 8192 TIFF,
NASA's 21,600 × 10,800 BMNG-monthly mosaics) exceed reasonable
single-asset bundle size; the path to that resolution range is
streaming Blue Marble tiles from NASA GIBS (planned follow-on,
tracked separately).

## Licence and attribution

NASA imagery is generally in the **public domain** ("NASA content —
images, audio, video, and computer files used in the rendition of
3-dimensional models, such as texture maps and polygon data in any
format — is in the public domain"). No fee or permission is required
for use; crediting NASA is courteous and the right thing to do.

When this dataset is visible in the rendered scene (i.e. whenever the
globe view is in front of the camera), the in-app attribution overlay
(plan 0001 M4) should include:

> Earth imagery: NASA Visible Earth (Blue Marble)

See: <https://www.nasa.gov/multimedia/guidelines/index.html>
