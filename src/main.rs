//! Native binary entry point.
//!
//! M0 stub — initialises logging and prints the version. The winit window
//! + wgpu surface + render loop land in M0's final pass.

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("aegis {} (native)", aegis::version::VERSION);
    println!("aegis {}", aegis::version::VERSION);
}

// The wasm build is driven through `lib.rs` / `web.rs`; this binary entry
// point is native-only and would never link on wasm32 because of `winit`.
#[cfg(target_arch = "wasm32")]
fn main() {}
