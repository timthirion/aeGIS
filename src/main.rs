//! Native binary entry point.

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("aegis {} (native)", aegis::version::VERSION);
    aegis::run();
}

// The wasm build is driven through `lib.rs` / `web.rs`; this binary entry
// point would never link on wasm32 because of `winit`.
#[cfg(target_arch = "wasm32")]
fn main() {}
