//! The Rust guest SDK for ikigai modules.
//!
//! - [`core_module`]: the **core-module (ikigai-module ABI v1) guest ABI**, the plain
//!   `extern "C"` exports a `wasm32-unknown-unknown` module presents and the synchronous
//!   `host_call` pump, so a fixture guest is a few lines (see [`ikigai_module!`]). The session is
//!   ikigai-module's, built from the wire-only mirrors re-exported here as [`abi`].
//! - [`component`]: the **component SDK** for the `ikigai:runtime` worlds (R-D13): wit-bindgen
//!   bindings for the `endpoint` world, so a component guest implements `describe` and `invoke`
//!   and calls the kernel through the `kernel` import.

extern crate alloc;

/// The session's wire types (`ikigai-module-abi`), the one definition host and guest share.
pub use ikigai_runtime::session::abi;

pub mod component;
pub mod core_module;
