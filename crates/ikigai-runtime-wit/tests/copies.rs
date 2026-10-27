//! The WIT is the deliverable of this crate, and two other crates carry a COPY of it, because a
//! published crate cannot read outside its own directory and both `bindgen!` macros read the
//! file at compile time: `ikigai-runtime-wasmtime/wit` (the host bindings) and
//! `ikigai-runtime-guest/wit` (the component SDK). This test keeps every copy byte-identical to
//! the canonical one, so the host and the guest SDK can never drift from the package.

use ikigai_runtime_wit::RUNTIME_WIT;

#[test]
fn every_copy_of_the_wit_is_identical_to_the_package() {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    for copy in ["ikigai-runtime-wasmtime", "ikigai-runtime-guest"] {
        let path = crates.join(copy).join("wit").join("runtime.wit");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            text,
            RUNTIME_WIT,
            "{} has drifted from ikigai-runtime-wit/wit/runtime.wit; copy it again",
            path.display()
        );
    }
}
