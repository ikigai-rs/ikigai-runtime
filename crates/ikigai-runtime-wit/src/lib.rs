//! The `ikigai:runtime@0.1.0` WIT package (R-D2), as files plus their text for embedding.
//!
//! The WIT itself lives in `wit/runtime.wit`; this crate ships it and exposes it so a host or a
//! build can embed or resolve it without locating the file on disk. The parse test
//! (`tests/parse.rs`) proves the package parses and that its worlds hold the interfaces this
//! record names.

/// The package name, as declared in the WIT.
pub const PACKAGE: &str = "ikigai:runtime@0.1.0";

/// The WIT source text of the package.
pub const RUNTIME_WIT: &str = include_str!("../wit/runtime.wit");
