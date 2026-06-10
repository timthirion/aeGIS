# Bundled TLE fixtures

## File

`iss-fixture.txt` — the ISS (ZARYA, NORAD 25544) Two-Line
Element set, fetched once from Celestrak and committed to keep
the offline-test path stable and the no-network native launch
honest.

## Source

Celestrak (`celestrak.org`). TLE format spec at
[celestrak.org/NORAD/documentation/tle-fmt.php](https://celestrak.org/NORAD/documentation/tle-fmt.php).

## License

TLEs are observational data and are **not subject to copyright**.
Celestrak's redistribution terms ask for a credit when bulk-
embedding catalogs; aeGIS surfaces `Orbital elements: CelesTrak`
in the page footer whenever the satellite overlay is active.

## Refresh

The bundled fixture is a snapshot. Live TLEs are fetched on
startup against the current Celestrak URLs; the fixture only
loads when network is unavailable or for offline tests.
