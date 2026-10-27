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

    // (fixture directory, target, built artifact). The components are wasm32-wasip2 cdylibs,
    // which the target's linker (wasm-component-ld) emits as components.
    const FIXTURES: &[(&str, &str, &str)] = &[
        ("jail-guest", "wasm32-unknown-unknown", "jail_guest.wasm"),
        ("p1-cmd", "wasm32-wasip1", "p1cmd.wasm"),
        ("comp-endpoint", "wasm32-wasip2", "comp_endpoint.wasm"),
        ("comp-calc", "wasm32-wasip2", "comp_calc.wasm"),
        ("comp-probe", "wasm32-wasip2", "comp_probe.wasm"),
    ];
    for (dir, target, artifact) in FIXTURES {
        let fixture = fixtures.join(dir);
        build_guest(
            &cargo,
            &fixture.join("Cargo.toml"),
            target,
            artifact,
            &out_dir,
        );
        println!("cargo:rerun-if-changed={}", fixture.join("src").display());
        println!(
            "cargo:rerun-if-changed={}",
            fixture.join("Cargo.toml").display()
        );
        if fixture.join("wit").exists() {
            println!("cargo:rerun-if-changed={}", fixture.join("wit").display());
        }
    }
    // Rebuild if the guest-shared crates change.
    for shared in ["ikigai-runtime-guest", "ikigai-runtime"] {
        let crate_dir = manifest_dir.join("..").join(shared);
        println!("cargo:rerun-if-changed={}", crate_dir.join("src").display());
        println!(
            "cargo:rerun-if-changed={}",
            crate_dir.join("Cargo.toml").display()
        );
    }
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir
            .join("..")
            .join("ikigai-runtime-guest")
            .join("wit")
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
