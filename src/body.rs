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
//! ### Fictional worlds (Middle-earth, etc.)
//!
//! The `Body` shape supports a fictional world cleanly — drop a new
//! static into this module and add it to `ALL_BODIES`. We considered
//! shipping Middle-earth and removed it: the canonical Tolkien-estate
//! maps aren't under any open licence (and fan-made derivatives are
//! tolerated, not licensed), so any bundled imagery would violate
//! the data-source policy in
//! [memory `project-data-sources`](../../.claude/projects/-Users-tt-src-aegis/memory/project_data_sources.md).
//! A genuinely CC-licensed fictional-world basemap (or a community
//! self-hosted tile source) would slot in via the same `Basemap`
//! definition Mars / Moon use.

use crate::tile::CHICAGO_LONLAT;

/// Stable identifier for a body. Used in `(BodyId, BasemapId)`
/// pairs that key the renderer's per-(body, basemap) state.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BodyId {
    Earth,
    Mars,
    Moon,
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

/// Atmospheric-scattering parameters for a body. Consumed by
/// `atmosphere.wgsl` (plan 0008 M1+M2). Bodies without a meaningful
/// atmosphere (the Moon) leave `Body::atmosphere` as `None` and the
/// scattering pipeline skips them entirely.
///
/// **Tuning convention.** The renderer treats every body as a unit
/// sphere (`planet_radius = 1.0`), so `atmosphere_radius` is the
/// only thickness knob; values are exaggerated above real-world
/// ratios so the halo reads visibly without a 6000× zoom. Likewise
/// `rayleigh_beta` / `mie_beta` are pre-multiplied by the
/// planet-radius-in-metres factor that would otherwise appear in
/// the optical-depth integral, so the shader can treat all
/// distances as normalised.
#[derive(Copy, Clone, Debug)]
pub struct Atmosphere {
    /// Outer-shell radius in normalised units (`planet_radius = 1.0`).
    /// Earth ~1.025 (a bit thicker than the real 1.016), Mars ~1.012.
    pub atmosphere_radius: f32,
    /// Per-wavelength Rayleigh extinction (R, G, B). Earth's blue
    /// sky comes from the third component being ~4× the first.
    pub rayleigh_beta: [f32; 3],
    /// Mie extinction (per-wavelength for tuning; usually all three
    /// equal for an Earth-like haze, slightly red-shifted on Mars).
    pub mie_beta: [f32; 3],
    /// Mie phase-function asymmetry. Positive = forward-scattering
    /// (haze glow near the sun). Earth uses ~0.76; Mars's coarser
    /// dust scatters more isotropically (~0.5).
    pub mie_g: f32,
    /// Top-of-atmosphere sun intensity in normalised units.
    pub sun_intensity: f32,
    /// Rayleigh density scale height (fraction of planet radius).
    /// Earth real ~0.00126; v1 uses larger values so the halo is
    /// thick enough to read at globe view.
    pub rayleigh_scale: f32,
    /// Mie density scale height.
    pub mie_scale: f32,
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
    /// Fragment-shader dim multiplier for the night side. The day
    /// side renders at 1.0; the night side is `mix(night_dim, 1.0,
    /// day)` where `day` is a smoothstep over `dot(sphere, sun_dir)`.
    /// Earth uses 0.15 (city-lights texture brings detail back on
    /// top); Moon uses 0.02 (essentially black — there's nothing
    /// for moonlight to scatter off); Mars sits in between at 0.10.
    /// Plan 0009 M0.
    pub night_dim: f32,
    /// Optional equirectangular city-lights texture, additively
    /// composited on top of the dimmed day-side surface on the
    /// night hemisphere. Earth uses NASA Black Marble; bodies
    /// without recognisable nightlights (Mars, Moon) leave it
    /// None and fall back to the plain `night_dim` darken. Plan
    /// 0009 M2.
    pub night_texture: Option<&'static [u8]>,
    /// Atmospheric-scattering parameters. None for airless bodies
    /// (Moon); the renderer skips the atmosphere draw entirely
    /// when this is None. Plan 0008.
    pub atmosphere: Option<Atmosphere>,
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

/// Compile-time bundled NASA Black Marble equirectangular night-
/// lights texture. 2048×1024 JPEG, ~220 KB. Sampled by `earth.wgsl`
/// on the night side. Plan 0009 M2; see `data/black-marble/README.md`.
const EARTH_NIGHT_BYTES: &[u8] = include_bytes!("../data/black-marble/black_marble_2048x1024.jpg");

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
    night_dim: 0.15,
    night_texture: Some(EARTH_NIGHT_BYTES),
    // Earth atmosphere — tuned for visible blue halo + reddish
    // terminator glow at globe view, not radiometrically accurate
    // (real atmosphere thickness ratio ~0.0157; v1 exaggerates to
    // 0.025 so the halo reads at canvas-pixel sizes).
    atmosphere: Some(Atmosphere {
        atmosphere_radius: 1.025,
        rayleigh_beta: [5.5, 13.0, 33.1],
        mie_beta: [21.0, 21.0, 21.0],
        mie_g: 0.76,
        sun_intensity: 18.0,
        rayleigh_scale: 0.008,
        mie_scale: 0.0014,
    }),
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
    night_dim: 0.10,
    night_texture: None,
    // Mars atmosphere — thin (real ~0.6% Earth's surface pressure)
    // and dust-tinted. Less Rayleigh, more red-shifted Mie haze;
    // smaller atmosphere shell. The look should read as "thin
    // reddish halo," not radiometrically accurate.
    atmosphere: Some(Atmosphere {
        atmosphere_radius: 1.012,
        rayleigh_beta: [16.0, 9.0, 4.5],
        mie_beta: [9.0, 6.0, 4.0],
        mie_g: 0.5,
        sun_intensity: 9.0,
        rayleigh_scale: 0.004,
        mie_scale: 0.0008,
    }),
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
    night_dim: 0.02,
    night_texture: None,
    atmosphere: None,
};

// A Middle-earth body was prototyped in plan 0003 M4 as a procedural
// placeholder and then removed: shipping it amounted to either (a) a
// made-up texture that wasn't actually Middle-earth (dishonest), or
// (b) bundling Tolkien-derived imagery without a real licence
// (against the data-source policy). The architecture supports a
// fourth body cleanly — see the module-level "Fictional worlds"
// note for how to wire one in once an open-licensed source exists.

static ALL_BODIES: &[&Body] = &[&EARTH, &MARS, &MOON];

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
