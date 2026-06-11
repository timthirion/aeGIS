# Black Marble (Earth at Night)

`black_marble_2048x1024.jpg` is NASA Earth Observatory's 2012 "Earth at Night"
composite (the famous "Black Marble"), downscaled from the 3600×1800 source to
2048×1024 and re-encoded as JPEG quality 70 to keep the bundled WebAssembly
payload small (~220 KB).

- **Source:** [NASA Earth Observatory — Night Lights 2012](https://earthobservatory.nasa.gov/images/79765/night-lights-2012-map),
  image record `dnb_land_ocean_ice.2012.3600x1800.jpg`.
- **Sensor:** Suomi NPP / VIIRS day-night band, composited from cloud-free
  observations over 22 days in April + October 2012.
- **License:** Public domain (NASA imagery is not subject to copyright in the
  United States; see [NASA Media Usage Guidelines](https://www.nasa.gov/multimedia/guidelines/index.html)).
- **Used by:** plan 0009 M2 (city-lights overlay on Earth's night side).

A 2016 Suomi NPP / VIIRS update exists (the "Black Marble 2016") at higher
resolution; we ship the 2012 version because the 2016 GeoTIFFs available for
free are radiometric DNB radiance data, not the colour-balanced sRGB JPEG
this renderer needs. If a 2016 sRGB composite becomes available under the
same NASA public-domain terms, swap it in via the same path.
