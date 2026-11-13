// The module's shadow stack: tsrs_wasm's 80 MiB (its notes/wasm-build.md census), so a deep
// expression traps instead of corrupting memory. Only for wasm targets.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let family = std::env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    if family.split(',').any(|f| f == "wasm") {
        println!("cargo:rustc-cdylib-link-arg=-zstack-size={}", 80u64 << 20);
    }
}
