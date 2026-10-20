use std::env;

/// Main-thread stack, matching the macOS / Linux default.
const MAIN_STACK_SIZE: u32 = 8 * 1024 * 1024;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Windows gives the main thread only 1 MiB, and the game loop runs on the main thread.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    match env::var("CARGO_CFG_TARGET_ENV").as_deref() {
        Ok("msvc") => println!("cargo:rustc-link-arg-bins=/STACK:{MAIN_STACK_SIZE}"),
        Ok("gnu") => println!("cargo:rustc-link-arg-bins=-Wl,--stack,{MAIN_STACK_SIZE}"),
        _ => {}
    }
}
