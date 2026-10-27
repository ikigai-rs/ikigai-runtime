//! The component SDK (R-D13): wit-bindgen bindings for the `ikigai:runtime` `endpoint` world, so
//! a component guest implements [`Guest`] (`describe` and `invoke`) and calls the kernel through
//! [`kernel`]. **Synchronous** p2 bindings (wit-bindgen's `async` feature is off): `invoke` runs to
//! completion, and each kernel call returns when the host has resolved it.
//!
//! ```ignore
//! use ikigai_runtime_guest::component::{kernel, Guest, Request, Representation, Error};
//!
//! struct Hello;
//! impl Guest for Hello {
//!     fn describe() -> String {
//!         r#"[{"id":"hello","title":"hello","summary":"Greet.","verbs":["Source"],"inputs":[],"outputs":["text/plain"]}]"#.into()
//!     }
//!     fn invoke(req: Request) -> Result<Representation, Error> {
//!         let name = kernel::source("urn:host:name", &[])?;
//!         Ok(Representation { media_type: "text/plain".into(), bytes: name.bytes })
//!     }
//! }
//! ikigai_runtime_guest::component::export_endpoint!(Hello);
//! ```
//!
//! `describe()` returns core `Description` JSON, one object or an array; each `id` is mounted at
//! `urn:runtime:app:{app}:{id}` and invoked with `Request { endpoint: id, args }`. The WIT is
//! `wit/runtime.wit`, a copy of `ikigai-runtime-wit`'s that its tests keep identical.
//!
//! A guest whose world has MORE than the `endpoint` world (its own exports, its own imports)
//! generates its own bindings with wit-bindgen against `ikigai-runtime-wit`'s WIT; these are for
//! the guest that is exactly an endpoint.

wit_bindgen::generate!({
    path: "wit",
    world: "ikigai:runtime/endpoint",
    pub_export_macro: true,
    export_macro_name: "export_endpoint",
    default_bindings_module: "ikigai_runtime_guest::component",
});

pub use exports::ikigai::runtime::endpoint_api::{Guest, Request};
pub use ikigai::runtime::kernel;
pub use ikigai::runtime::kernel::{Arg, Error, Representation};
