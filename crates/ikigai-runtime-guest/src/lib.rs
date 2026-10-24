//! The Rust guest SDK for ikigai modules.
//!
//! R3 implements the **core-module (ikigai-module ABI v1) guest ABI** — the plain `extern "C"`
//! exports a `wasm32-unknown-unknown` module presents, and the synchronous `host_call` pump — so
//! a fixture guest is a few lines (see [`ikigai_module!`]). The wit-bindgen component SDK for the
//! `endpoint` and `app` worlds (R-D13) lands with the component tier in R3b.

pub mod core_module;
