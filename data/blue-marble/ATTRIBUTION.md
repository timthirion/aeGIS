# Blue Marble Earth imagery

`earth_2048x1024.jpg` is the canonical NASA **Blue Marble: Land
Surface, Shallow Water, and Shaded Topography** (`land_shallow_topo_2048`)
imagery, unmodified, an open dataset by NASA / Goddard Space Flight
Center / Reto Stöckli.

## Provenance

- Source URL: <https://eoimages.gsfc.nasa.gov/images/imagerecords/57000/57752/land_shallow_topo_2048.jpg>
- NASA's catalog entry: <https://visibleearth.nasa.gov/images/57752/blue-marble-land-surface-shallow-water-and-shaded-topography>
- Bundled size: 2048 × 1024 JPEG (≈ 240 KB) — same bytes NASA serves.

We previously bundled a 1024×512 PNG (≈ 450 KB after format
conversion). The larger native-resolution source is *smaller*
because JPEG compresses photographs more efficiently than PNG, and
gives 4× the pixel density on screen so the globe view stays sharp
under closer cropping. WebGPU's downlevel WebGL2 limit caps
`max_texture_dimension_2d` at 2048, so 2048×1024 is the largest
equirectangular texture we can ship without dropping that
compatibility floor.

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
