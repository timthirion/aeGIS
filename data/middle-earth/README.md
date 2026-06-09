# Middle-earth (placeholder)

## File

`middle_earth_2048x1024.jpg` — a procedurally-generated
equirectangular placeholder (~37 KB) that shows two fictional
continents in earth-tones with suggestion of mountains, forests,
and polar ice. **It is not a map of Tolkien's Middle-earth** and
makes no attempt to depict any canonical place names, regions, or
geographic features from Tolkien's works.

The file lives here so the Middle-earth body has *something* to
render before tile imagery is wired up. The architectural slot
exists (`body::MIDDLE_EARTH` lands in plan 0003 M4); a real
Middle-earth basemap would require a CC-licensed tile source that
respects the Tolkien estate's copyright on the canonical maps.

## Source

Generated locally by the `python3 -c '...'` inline script in
plan 0003 M4's commit. The script uses Pillow primitives only
(ellipses + Gaussian blur) so it's reproducible. No third-party
art was used.

## License

This placeholder is original work by the aeGIS project, released
under the same Apache-2.0 licence as the rest of the repository.
Replace it with a CC-licensed Middle-earth-style map if you wire
one up; the architecture is ready.
