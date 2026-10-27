//! The **component tier** on wasmtime (R-D1, R-D2, R-D4, R-D5, R-D7, R-D10).
//!
//! A [`ComponentEngine`] compiles a component artifact once (cached by digest and the engine's
//! compatibility hash) and [`mount`](ComponentEngine::mount)s it as an app: a core `Space`
//! ([`ComponentSpace`]) answering only under `urn:runtime:app:{app}:`.
//!
//! - **Worlds** ([`World`]). `endpoint` runs with NO WASI authority: whatever WASI the component
//!   imports is linked against an empty, deterministic context (no files, env, args, sockets or
//!   DNS; stdio captured and bounded; a fixed clock; a seeded RNG), whatever the caller holds.
//!   `app` gets the WASI a grant unlocks, built per call from `ceiling ∩ caller` through the
//!   grant → WASI plan (R-D3); a socket or `wasi:http` interface no grant opens is linked to a
//!   refusal, so a call to it fails at the import, typed `Denied`.
//! - **Projection** (R-D4): each exported function is an endpoint, a Source unless declared a
//!   Sink; scalars map to XSD, structures to JSON ([`values`](self) has the table); `describe()`
//!   comes from the WIT docs the binary carries, or the guest's own `endpoint-api` `describe()`.
//!   Handles, streams and futures are skipped and reported ([`LoadReport`]).
//! - **Imports** (R-D5): `ikigai:runtime/kernel` is the runtime's; `wasi:http` goes to the
//!   kernel's `urn:http:*`; every other import is bound by configuration ([`ImportBindings`]) or
//!   refuses the mount as `Unresolved`. Every guest request runs through the invocation that is
//!   running it, under the session's capability: traced, golden-threaded, denied like any other.
//! - **Budgets** (R-D10): fuel, epoch and memory, the meet of the engine ceiling, the app's
//!   limits and the caller's `urn:cap:compute:*` grants, with the same error mapping as the
//!   core-module tier.
//!
//! ★ **Where a projected result becomes a `Representation`** is `values::val_to_representation`
//! (and the `ApiEndpoint` answer in `space`): the one seam ledger #2247 replaces to project
//! `stream<u8>` and `stream<T>` onto core's `Body`, not built here.

mod bindings;
mod engine;
mod http;
mod imports;
mod project;
mod space;
mod values;

pub use engine::{AppConfig, ComponentEngine, LoadReport, World};
pub use imports::{ImportBinding, ImportBindings};
pub use project::Skipped;
pub use space::ComponentSpace;
