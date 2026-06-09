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

/// Which tile-grid pyramid a basemap uses.
///
/// **WebMercator**: the classic slippy XYZ. At zoom `z` the world is
/// `2^z × 2^z` tiles covering `[-180°, +180°]` longitude and
/// `[-MERCATOR_LAT_MAX, +MERCATOR_LAT_MAX]` (`≈ ±85.05°`)
/// latitude. The Mercator projection's distortion at the poles is
/// why the cap latitude exists. Earth Carto + Esri use this.
///
/// **Equirectangular**: Plate Carrée. At zoom `z` the world is
/// `2 * 2^z × 2^z` tiles (note the 2:1 aspect ratio) covering
/// `[-180°, +180°]` × `[-90°, +90°]` with no distortion. NASA Trek
/// uses this for Mars / Moon. Tile addresses + the shader's
/// `world → lonlat` inverse both differ from WebMercator.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TileProjection {
    WebMercator,
    Equirectangular,
}

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

// Mars, Moon, and Middle-earth statics arrive in M2 / M3 / M4. For
// now the renderer only knows Earth, so M0 is a pure refactor — no
// visible behaviour change.

static ALL_BODIES: &[&Body] = &[&EARTH];

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
