//! The wasmtime implementation of the ikigai runtime: both tiers (R-D1).
//!
//! **The core-module tier** ([`WasmtimeCoreEngine`]) runs plain core modules under three calling
//! conventions, chosen per module from its exports ([`detect_convention`]):
//! 1. **numeric signatures** directly;
//! 2. the **ikigai-module ABI v1** (`ikigai_invoke` + `host_call`, ikigai-module's postcard
//!    session) — a fresh `Store` per session, with every `host_call` held to the session's
//!    capability; [`JailTransport`] mounts such a module into a kernel through ikigai-module's
//!    own `ModuleSpace`;
//! 3. **WASI p1 command modules** (stdin → stdout), under an empty, deterministic context.
//!
//! **The component tier** ([`component`]) runs WebAssembly components: the `ikigai:runtime`
//! worlds `endpoint` (no WASI authority) and `app` (the WASI a grant unlocks, built per call from
//! the grant → WASI plan), each export projected as an ikigai endpoint under
//! `urn:runtime:app:{app}:…` (R-D4) and each import bound to a kernel request at run time under
//! the caller's capability (R-D5).
//!
//! Budgets (fuel · epoch · memory) come from [`Limits`](ikigai_runtime::Limits) as the meet of
//! the engine's host ceiling and the caller's grant; the error mapping is R-D10 (fuel and memory
//! → `OverBudget`, never transient; a budget deadline → `OverBudget`, a host deadline →
//! `Timeout`). Both tiers run **asynchronously**: a guest's kernel call awaits the kernel while
//! the guest itself sees a synchronous call (wasmtime fibers; `Config::async_support` is a no-op
//! in wasmtime 50 and is never called).

mod bridge;
mod budget;
pub mod component;
mod core_tier;
mod transport;
pub mod wasi;

pub use budget::Metering;
pub use core_tier::{detect_convention, Compiled, WasmtimeCoreEngine};
pub use ikigai_runtime::engine::Invoked;
pub use transport::JailTransport;

use ikigai_runtime::loader::EngineCompatHash;

/// The engine-compatibility hash a compiled form is keyed by (R-D7), as a stable string.
pub(crate) fn compat_hash(engine: &wasmtime::Engine) -> EngineCompatHash {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    engine.precompile_compatibility_hash().hash(&mut hasher);
    EngineCompatHash::new(format!("wasmtime-50-{:016x}", hasher.finish()))
}
