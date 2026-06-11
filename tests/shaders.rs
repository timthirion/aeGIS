//! WGSL validation tests. Every `.wgsl` file shipped in the crate gets
//! parsed + validated by `naga` here so shader regressions surface in
//! `cargo test`, not at runtime when a GPU device tries to compile them.
//!
//! See `AGENTS.md` testing rule #3.

use naga::front::wgsl;
use naga::valid::{Capabilities, ValidationFlags, Validator};

fn validate_wgsl(source: &str, label: &str) {
    let module = match wgsl::parse_str(source) {
        Ok(m) => m,
        Err(e) => panic!(
            "WGSL parse failed for {label}:\n{}",
            e.emit_to_string(source)
        ),
    };

    let mut validator = Validator::new(ValidationFlags::all(), Capabilities::all());
    if let Err(e) = validator.validate(&module) {
        panic!("WGSL validation failed for {label}: {e:?}");
    }
}

#[test]
fn tile_shader_validates() {
    validate_wgsl(include_str!("../src/shaders/tile.wgsl"), "tile.wgsl");
}

#[test]
fn vector_shader_validates() {
    validate_wgsl(include_str!("../src/shaders/vector.wgsl"), "vector.wgsl");
}

#[test]
fn caps_shader_validates() {
    validate_wgsl(include_str!("../src/shaders/caps.wgsl"), "caps.wgsl");
}

#[test]
fn earth_shader_validates() {
    validate_wgsl(include_str!("../src/shaders/earth.wgsl"), "earth.wgsl");
}

#[test]
fn orbit_shader_validates() {
    validate_wgsl(include_str!("../src/shaders/orbit.wgsl"), "orbit.wgsl");
}

#[test]
fn orbit_trail_shader_validates() {
    validate_wgsl(
        include_str!("../src/shaders/orbit_trail.wgsl"),
        "orbit_trail.wgsl",
    );
}

#[test]
fn atmosphere_shader_validates() {
    validate_wgsl(
        include_str!("../src/shaders/atmosphere.wgsl"),
        "atmosphere.wgsl",
    );
}
