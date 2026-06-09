//! Body abstraction — Earth, Mars, Moon, and the architectural slot
//! for fictional worlds like Middle-earth.
//!
//! The renderer's camera math is already body-agnostic (everything
//! projects to a unit sphere); a `Body` collects the parts that
//! *do* vary across worlds — the basemap URLs, the default camera,
//! the fallback texture, the polar-cap colours, and the per-body
//! switches like "show country outlines?" (Earth: yes; Mars: no).
//!
//! ### Why a struct + statics, not an enum
//!
//! The earlier design used a `BasemapMode { Map, Satellite }` enum
//! and hard-coded the URL templates in `tile.rs`. That works for one
//! body. For four, every site that mentions a URL would grow a
//! per-body branch. Modelling each body as a `static Body` keeps
//! the per-body data in one place — the basemaps array — and the
//! renderer reads through it. Adding a fifth body becomes "add a
//! static" rather than "thread a new enum variant through six files."
//!
//! ### Middle-earth
//!
//! Tolkien's estate aggressively enforces copyright on derivative
//! Middle-earth maps; the published canonical maps are not under
//! any open license. Plan 0003 reserves an architectural slot for
//! Middle-earth but does not bundle tile imagery. A
//! community-supplied Middle-earth basemap can be dropped in via
//! the same `Basemap` shape the real bodies use; the README
//! documents the contributor path.

use crate::tile::CHICAGO_LONLAT;

/// Stable identifier for a body. Used in `(BodyId, BasemapId)`
/// pairs that key the renderer's per-(body, basemap) state.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BodyId {
    Earth,
    Mars,
    Moon,
    /// Architectural slot for a fictional world. v1 ships a
    /// placeholder fallback texture and a "drop your tiles here"
    /// pointer; community tile sources can be wired in via the
    /// same `Basemap` shape as the real bodies.
    MiddleEarth,
}

/// Stable identifier for a basemap *within* a body. The string is
/// the URL-safe slug used in URL state ("map", "satellite", "color",
/// "terrain", ...). Keeping it as a thin newtype rather than a free
/// `&str` makes the renderer's `(BodyId, BasemapId)` pair type-safe.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct BasemapId(pub &'static str);

// TileProjection moved to `crate::tile` in plan 0003 M1 — it's a
// tile-pyramid concept rather than a body property. Re-exported via
// the `pub use` below so existing callers don't break.
pub use crate::tile::TileProjection;

/// One basemap option a user can select for a body. Multiple
/// basemaps per body let the UI do "Map / Satellite" on Earth and
/// "Color / Terrain" on Mars.
pub struct Basemap {
    pub id: BasemapId,
    /// User-facing label rendered in the basemap toggle.
    pub display_name: &'static str,
    pub projection: TileProjection,
    /// URL template with `{z}`, `{x}`, `{y}` substitutions. The
    /// order matters — Esri / NASA Trek both use `{z}/{y}/{x}`,
    /// while slippy + Carto use `{z}/{x}/{y}`. Encoded literally
    /// so the renderer doesn't have to know which convention
    /// each provider follows.
    pub url_template: &'static str,
    /// Maximum zoom this basemap publishes worldwide. Past this,
    /// the camera keeps zooming but tile resolution stays at the
    /// cap.
    pub max_z: u8,
    /// HTML attribution string, rendered into the page footer when
    /// this basemap is active. The licences themselves are
    /// documented in `README.md`.
    pub attribution_html: &'static str,
    /// Polar-cap colours specifically for this basemap. Earth's
    /// satellite basemap wants realistic ice white; Earth's Carto
    /// basemap wants the stylised palette. Mars wants dust red.
    pub cap_colors: CapColors,
}

/// Polar-cap colour pair. Stored as `sRGB8` since the renderer
/// already round-trips through `srgb8_to_linear_rgba` for every
/// cap write.
#[derive(Copy, Clone, Debug)]
pub struct CapColors {
    /// `[r, g, b, a]` sRGB8 for the north cap.
    pub north: [u8; 4],
    /// `[r, g, b, a]` sRGB8 for the south cap.
    pub south: [u8; 4],
}

/// Default camera state for a fresh load on a body. The fly-to
/// from M3 lets a body switch also smoothly glide to this state.
#[derive(Copy, Clone, Debug)]
pub struct HomeView {
    pub lon: f64,
    pub lat: f64,
    pub zoom: f64,
}

/// A spherical body the renderer can show. Everything that varies
/// per-body lives here.
pub struct Body {
    pub id: BodyId,
    /// User-facing label rendered in the body switcher tooltip.
    pub display_name: &'static str,
    /// Single-character (usually emoji) glyph rendered in the body
    /// switcher button. Falls back to text if the user's font
    /// stack has no glyph for it.
    pub icon: &'static str,
    /// Equatorial radius in metres. Used by future precision work
    /// (search-result distance, ellipsoidal upgrades). The renderer
    /// itself treats every body as a unit sphere in 3D.
    pub equatorial_radius_m: f64,
    /// Available basemaps for this body. At least one; the first
    /// entry is the default on body-switch.
    pub basemaps: &'static [Basemap],
    pub home: HomeView,
    /// Fallback equirectangular texture (JPEG bytes embedded at
    /// compile time) shown beneath the sphere before tiles stream
    /// in. Per-body so the globe-view first paint is recognisable
    /// instead of always being a blue Earth.
    pub fallback_texture: &'static [u8],
    /// Whether the Natural Earth political-overlay (country
    /// outlines) layer should render. False for everything except
    /// Earth — Mars has no countries.
    pub show_political_overlays: bool,
}

impl Body {
    /// Look up a basemap on this body by id, panicking on miss.
    /// The miss case is a programmer error — `BasemapId` comes from
    /// either the `basemaps` array directly or the URL state that
    /// was previously written from one. Returning `&Basemap` rather
    /// than `Option` keeps the call sites tidy.
    pub fn basemap(&self, id: BasemapId) -> &Basemap {
        self.basemaps
            .iter()
            .find(|b| b.id == id)
            .unwrap_or_else(|| {
                panic!(
                    "body {:?} has no basemap '{}' — available: {:?}",
                    self.id,
                    id.0,
                    self.basemaps.iter().map(|b| b.id.0).collect::<Vec<_>>()
                )
            })
    }

    /// The default basemap on this body (first in the array).
    pub fn default_basemap(&self) -> BasemapId {
        self.basemaps[0].id
    }
}

// ---------------------------------------------------------------------------
// Per-body statics. Each `Body` lives as a `static` so call sites can hand
// out `&'static Body` freely without lifetime gymnastics.
// ---------------------------------------------------------------------------

/// Compile-time bundled Blue Marble equirectangular Earth texture.
/// 4096×2048 JPEG, ~3 MB. Shown beneath the sphere before satellite
/// tiles stream in.
const EARTH_FALLBACK_BYTES: &[u8] = include_bytes!("../data/blue-marble/earth_4096x2048.jpg");

const EARTH_BASEMAPS: &[Basemap] = &[
    Basemap {
        id: BasemapId("map"),
        display_name: "Map",
        projection: TileProjection::WebMercator,
        url_template: "https://a.basemaps.cartocdn.com/rastertiles/voyager/{z}/{x}/{y}@2x.png",
        max_z: 19,
        attribution_html: "© <a href=\"https://www.openstreetmap.org/copyright\">OpenStreetMap</a> contributors, © <a href=\"https://carto.com/attributions\">CARTO</a>",
        cap_colors: CapColors {
            north: [170, 206, 212, 255],
            south: [246, 239, 229, 255],
        },
    },
    Basemap {
        id: BasemapId("satellite"),
        display_name: "Satellite",
        projection: TileProjection::WebMercator,
        url_template: "https://services.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
        max_z: 19,
        attribution_html: "Source: <a href=\"https://www.esri.com/en-us/legal/copyright-trademarks\">Esri</a>, Maxar, Earthstar Geographics, and the GIS User Community",
        cap_colors: CapColors {
            north: [190, 215, 230, 255],
            south: [248, 250, 252, 255],
        },
    },
];

pub static EARTH: Body = Body {
    id: BodyId::Earth,
    display_name: "Earth",
    icon: "🌍",
    equatorial_radius_m: 6_378_137.0,
    basemaps: EARTH_BASEMAPS,
    home: HomeView {
        lon: CHICAGO_LONLAT.0,
        lat: CHICAGO_LONLAT.1,
        zoom: 11.0,
    },
    fallback_texture: EARTH_FALLBACK_BYTES,
    show_political_overlays: true,
};

// --- Mars -----------------------------------------------------------------

const MARS_FALLBACK_BYTES: &[u8] = include_bytes!("../data/mars/mars_2048x1024.jpg");

const MARS_BASEMAPS: &[Basemap] = &[
    Basemap {
        id: BasemapId("color"),
        display_name: "Color",
        projection: TileProjection::Equirectangular,
        url_template: "https://trek.nasa.gov/tiles/Mars/EQ/Mars_Viking_MDIM21_ClrMosaic_global_232m/1.0.0/default/default028mm/{z}/{y}/{x}.jpg",
        // NASA Trek's Viking colour mosaic pyramid publishes through
        // z=7 globally. Past that the imagery is just stretch.
        max_z: 7,
        attribution_html: "Imagery: <a href=\"https://trek.nasa.gov/mars/\">NASA</a> / Viking MDIM 2.1",
        cap_colors: CapColors {
            // Mars polar caps are CO2 + water ice with dust; both
            // poles appear near-white. Match the unprojected fallback
            // texture pole pixels.
            north: [240, 235, 228, 255],
            south: [240, 235, 228, 255],
        },
    },
    Basemap {
        id: BasemapId("terrain"),
        display_name: "Terrain",
        projection: TileProjection::Equirectangular,
        url_template: "https://trek.nasa.gov/tiles/Mars/EQ/Mars_MGS_MOLA_ClrShade_merge_global_463m/1.0.0/default/default028mm/{z}/{y}/{x}.jpg",
        max_z: 6,
        attribution_html: "Topography: <a href=\"https://trek.nasa.gov/mars/\">NASA</a> / MGS MOLA Color Hillshade",
        cap_colors: CapColors {
            north: [180, 180, 188, 255],
            south: [180, 180, 188, 255],
        },
    },
];

pub static MARS: Body = Body {
    id: BodyId::Mars,
    display_name: "Mars",
    icon: "🔴",
    equatorial_radius_m: 3_396_200.0,
    basemaps: MARS_BASEMAPS,
    home: HomeView {
        // Olympus Mons — the largest volcano in the solar system.
        // 226.2°E in the NASA +East convention normalises to -133.8°
        // in our (-180, +180) form.
        lon: -133.8,
        lat: 18.65,
        zoom: 4.0,
    },
    fallback_texture: MARS_FALLBACK_BYTES,
    show_political_overlays: false,
};

// --- Moon -----------------------------------------------------------------

const MOON_FALLBACK_BYTES: &[u8] = include_bytes!("../data/moon/moon_2048x1024.jpg");

const MOON_BASEMAPS: &[Basemap] = &[
    Basemap {
        id: BasemapId("mosaic"),
        display_name: "Mosaic",
        projection: TileProjection::Equirectangular,
        url_template: "https://trek.nasa.gov/tiles/Moon/EQ/LRO_WAC_Mosaic_Global_303ppd_v02/1.0.0/default/default028mm/{z}/{y}/{x}.jpg",
        max_z: 7,
        attribution_html: "Imagery: <a href=\"https://trek.nasa.gov/moon/\">NASA</a> / LRO LROC WAC",
        cap_colors: CapColors {
            // Lunar regolith — neutral grey, slightly cooler at the
            // poles where the limb-grazing illumination skews
            // shadows blue.
            north: [180, 180, 188, 255],
            south: [180, 180, 188, 255],
        },
    },
];

pub static MOON: Body = Body {
    id: BodyId::Moon,
    display_name: "Moon",
    icon: "🌙",
    equatorial_radius_m: 1_737_400.0,
    basemaps: MOON_BASEMAPS,
    home: HomeView {
        // Apollo 11 landing site — Mare Tranquillitatis.
        lon: 23.47,
        lat: 0.67,
        zoom: 4.0,
    },
    fallback_texture: MOON_FALLBACK_BYTES,
    show_political_overlays: false,
};

// --- Middle-earth (placeholder) -----------------------------------------

const MIDDLE_EARTH_FALLBACK_BYTES: &[u8] =
    include_bytes!("../data/middle-earth/middle_earth_2048x1024.jpg");

const MIDDLE_EARTH_BASEMAPS: &[Basemap] = &[
    Basemap {
        id: BasemapId("placeholder"),
        display_name: "Placeholder",
        projection: TileProjection::Equirectangular,
        // No real tile pyramid — the URL template points at a
        // local pseudo-source. Tile fetches will 404 and fall
        // through to retry-then-fail; the fallback texture is the
        // only thing the user actually sees.
        url_template: "/middle-earth-placeholder/{z}/{y}/{x}.jpg",
        max_z: 0,
        attribution_html: "Procedural placeholder. Not derived from Tolkien's canonical maps; see <code>data/middle-earth/README.md</code>.",
        cap_colors: CapColors {
            // Match the fallback's polar-ice tone so caps blend
            // cleanly into the unprojected texture.
            north: [220, 220, 230, 255],
            south: [200, 200, 210, 255],
        },
    },
];

pub static MIDDLE_EARTH: Body = Body {
    id: BodyId::MiddleEarth,
    display_name: "Middle-earth",
    icon: "🧙",
    // Approximating from the Arda map scaling — Middle-earth is
    // ~5400 km wide. Treating that as the equatorial radius gives
    // the camera reasonable scale relative to Earth / Mars / Moon.
    // The number doesn't influence rendering (we always treat the
    // body as a unit sphere); kept here so future precision work
    // has something honest to hand.
    equatorial_radius_m: 5_400_000.0 / std::f64::consts::PI,
    basemaps: MIDDLE_EARTH_BASEMAPS,
    home: HomeView {
        // Roughly centred on the placeholder's east continent — no
        // canonical Middle-earth coordinate is honoured here.
        lon: 60.0,
        lat: 30.0,
        zoom: 2.0,
    },
    fallback_texture: MIDDLE_EARTH_FALLBACK_BYTES,
    show_political_overlays: false,
};

static ALL_BODIES: &[&Body] = &[&EARTH, &MARS, &MOON, &MIDDLE_EARTH];

/// All bodies the renderer can currently render. Used by the body-
/// switcher UI to enumerate options.
pub fn all() -> &'static [&'static Body] {
    ALL_BODIES
}

/// Look up a body by id, panicking on miss (same reasoning as
/// `Body::basemap`).
pub fn by_id(id: BodyId) -> &'static Body {
    all()
        .iter()
        .find(|b| b.id == id)
        .unwrap_or_else(|| panic!("no body for id {id:?}"))
}

/// Format `{z}`, `{x}`, `{y}` substitutions in a basemap's URL
/// template against the given tile address. Used by the renderer's
/// per-body tile fetcher.
pub fn format_tile_url(template: &str, z: u8, x: u32, y: u32) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            // Read until '}'.
            let mut name = String::new();
            while let Some(&n) = chars.peek() {
                chars.next();
                if n == '}' {
                    break;
                }
                name.push(n);
            }
            match name.as_str() {
                "z" => out.push_str(&z.to_string()),
                "x" => out.push_str(&x.to_string()),
                "y" => out.push_str(&y.to_string()),
                other => {
                    out.push('{');
                    out.push_str(other);
                    out.push('}');
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earth_has_map_and_satellite_basemaps() {
        assert_eq!(EARTH.basemaps.len(), 2);
        assert_eq!(EARTH.basemaps[0].id, BasemapId("map"));
        assert_eq!(EARTH.basemaps[1].id, BasemapId("satellite"));
    }

    #[test]
    fn earth_default_basemap_is_map_unless_changed() {
        // Earth's first basemap is "map" — but the renderer's
        // *initial* mode is Satellite per the plan 0002 default-
        // view change. The body's default is what a fresh
        // body-switch lands on; the renderer's initial mode is a
        // separate decision.
        assert_eq!(EARTH.default_basemap(), BasemapId("map"));
    }

    #[test]
    fn body_basemap_lookup_finds_the_right_one() {
        let map = EARTH.basemap(BasemapId("map"));
        let sat = EARTH.basemap(BasemapId("satellite"));
        assert!(map.url_template.contains("cartocdn"));
        assert!(sat.url_template.contains("arcgisonline"));
    }

    #[test]
    #[should_panic(expected = "has no basemap")]
    fn body_basemap_lookup_panics_on_miss() {
        EARTH.basemap(BasemapId("does-not-exist"));
    }

    #[test]
    fn format_tile_url_substitutes_correctly() {
        let url = format_tile_url("https://example.com/{z}/{x}/{y}.png", 5, 9, 12);
        assert_eq!(url, "https://example.com/5/9/12.png");
    }

    #[test]
    fn format_tile_url_handles_esri_yx_order() {
        let url = format_tile_url(
            "https://services.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/{z}/{y}/{x}",
            5,
            9,
            12,
        );
        assert_eq!(
            url,
            "https://services.arcgisonline.com/ArcGIS/rest/services/World_Imagery/MapServer/tile/5/12/9"
        );
    }

    #[test]
    fn format_tile_url_preserves_unknown_substitutions() {
        // {q} isn't a recognised placeholder; leave it unchanged
        // so a Photon-style query template (if ever reused) works.
        let url = format_tile_url("https://example.com/{z}?q={q}", 3, 0, 0);
        assert_eq!(url, "https://example.com/3?q={q}");
    }

    #[test]
    fn all_bodies_includes_earth() {
        let bodies = all();
        assert!(bodies.iter().any(|b| b.id == BodyId::Earth));
    }

    #[test]
    fn by_id_round_trips() {
        let earth = by_id(BodyId::Earth);
        assert_eq!(earth.id, BodyId::Earth);
        assert_eq!(earth.display_name, "Earth");
    }
}
