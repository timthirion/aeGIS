//! aeGIS — open-source GIS in Rust, native + WebGPU.
//!
//! The library crate. Owns the renderer, the layer model, the CRS subsystem,
//! and the WGSL pipelines. Platform-agnostic; thin native and web entry
//! points drive it.
//!
//! See `AGENTS.md` and `plans/ROADMAP.md` for the project's direction.

pub mod version;

#[cfg(target_arch = "wasm32")]
pub mod web;
