//! Build the guest fixtures when the `jail-fixtures` feature is on, so the end-to-end tests have
//! real wasm to load. The default build does nothing here, needs no wasm toolchain, and is what
//! the shared CI jobs run; the dedicated `jail-fixtures` job adds the wasm targets and enables
//! the feature (9c).
//!
//! Each guest is built with a SEPARATE `--target-dir` under `OUT_DIR`, so the inner cargo never
//! contends for the outer build's target-dir lock (field guide: never share a target dir), and
//! the resulting `.wasm` is copied next to the test binaries via `OUT_DIR`.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    // Only build the fixtures under the feature; a plain build/clippy/doc skips the wasm toolchain.
    if env::var_os("CARGO_FEATURE_JAIL_FIXTURES").is_none() {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.join("..").join("..");
    let fixtures = workspace_root.join("fixtures");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    build_guest(
        &cargo,
        &fixtures.join("jail-guest").join("Cargo.toml"),
        "wasm32-unknown-unknown",
        "jail_guest.wasm",
        &out_dir,
    );
    build_guest(
        &cargo,
        &fixtures.join("p1-cmd").join("Cargo.toml"),
        "wasm32-wasip1",
        "p1cmd.wasm",
        &out_dir,
    );

    println!(
        "cargo:rerun-if-changed={}",
        fixtures
            .join("jail-guest")
            .join("src")
            .join("lib.rs")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        fixtures
            .join("p1-cmd")
            .join("src")
            .join("main.rs")
            .display()
    );
    // Rebuild if the guest-shared crates change.
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir
            .join("..")
            .join("ikigai-runtime-guest")
            .join("src")
            .display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir
            .join("..")
            .join("ikigai-runtime")
            .join("src")
            .display()
    );
}

fn build_guest(cargo: &str, manifest: &Path, target: &str, artifact: &str, out_dir: &Path) {
    let guest_target_dir = out_dir.join("guest-target");
    let status = Command::new(cargo)
        .args([
            "build",
            "--release",
            "--target",
            target,
            "--manifest-path",
            manifest.to_str().unwrap(),
            "--target-dir",
            guest_target_dir.to_str().unwrap(),
        ])
        // Do not inherit the outer build's target-dir or feature selection.
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn cargo for {}: {e}", manifest.display()));
    assert!(
        status.success(),
        "building fixture {} failed",
        manifest.display()
    );

    let built = guest_target_dir.join(target).join("release").join(artifact);
    let dest = out_dir.join(artifact);
    std::fs::copy(&built, &dest)
        .unwrap_or_else(|e| panic!("copying {} to {}: {e}", built.display(), dest.display()));
}
