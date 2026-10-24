//! The engine-neutral, **wasm-clean** core of the ikigai runtime.
//!
//! A WebAssembly artifact is a resource, loaded by digest and run with no authority at all;
//! every authority it gets is an ikigai capability mapped onto the engine's own mechanisms or
//! served by the kernel itself (the design record is `claude/runtime/DECISIONS.md` in
//! `ikigai-devtools`, ledger #2165). This crate holds the parts that do not depend on a
//! particular engine and must compile to `wasm32` (for the in-page delivery):
//!
//! - [`Limits`] — the fuel / epoch / memory budget and its **meet** (R-D10);
//! - [`grants`] — the capability families and the **grant → WASI plan**, a pure function
//!   tested as a table (R-D3);
//! - [`CoreEngine`] — the core-module engine trait, with wasmtime first and wasmi next (R-D1);
//! - [`session`] — the ikigai-module **ABI v1** session types and the `u64` packing (R-D6);
//! - [`projection`] — the names and verbs a module's exports project to (R-D4);
//! - [`loader`] — the content-addressed artifact atom and the compiled-cache key (R-D7);
//! - [`space`] — the adapter that presents a loaded module as resolvable endpoints.
//!
//! The runtime speaks ikigai-core's [`Representation`], [`Error`] (including `OverBudget`) and
//! [`Capability`] (re-exported below); it does not reinvent them.

pub mod engine;
pub mod grants;
pub mod limits;
pub mod loader;
pub mod projection;
pub mod session;
pub mod space;

pub use engine::{Call, Consumed, CoreEngine, EngineError, HostResolver, Output, Value};
pub use grants::{wasi_plan, ClockAccess, FsAccess, Grants, NetAccess, RandomAccess, WasiPlan};
pub use limits::{Deadline, Limits, Provenance};
pub use loader::{Artifact, ArtifactDigest, CompiledKey, EngineCompatHash, LoadError};
pub use projection::{ProjectedEndpoint, Verb};
pub use session::{pack, unpack, ModuleCall, ModuleReply};
pub use space::LoadedModule;

// Re-exports so a host links one set of kernel types through the runtime.
pub use ikigai_core::{Capability, Error, Representation};
