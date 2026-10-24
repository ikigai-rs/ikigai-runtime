//! The core-module engine trait (R-D1) and the host seam it calls.
//!
//! The core-module tier runs plain core modules through [`CoreEngine`]. wasmtime is the first
//! implementation (`ikigai-runtime-wasmtime`); wasmi is next, because it also compiles to wasm32
//! and so can run in a page. Both present the same thing to the kernel: a space whose endpoints
//! resolve — the kernel never learns which engine ran.
//!
//! R-D1 names four operations (compile, instantiate, call, consumed). In this R3 shape
//! `instantiate` is internal to [`CoreEngine::invoke`] — a fresh `Store` per session is the
//! strongest jail (R-D7) — and the consumption is returned from the same call as [`Consumed`],
//! so the trait that a second engine must satisfy is exactly `compile` + `invoke`.

use crate::limits::Limits;
use ikigai_core::{Capability, Error};
use std::sync::Arc;

/// A numeric wasm value, for the numeric-signature convention (R-D6 point 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

/// How a core module is invoked — one variant per calling convention (R-D6). The engine chooses
/// the convention per module from its exports at load time.
pub enum Call<'a> {
    /// A function whose parameters and results are all numeric, invoked directly.
    Numeric {
        /// The export to call.
        export: &'a str,
        args: &'a [Value],
    },
    /// The ikigai-module ABI v1 (`ikigai_invoke`): one postcard-encoded `ModuleCall` in, one
    /// `ModuleReply` out, with `host_call`s serviced mid-session (R-D6 point 2).
    Session {
        /// The postcard-encoded [`ModuleCall`](crate::session::ModuleCall).
        encoded_call: &'a [u8],
    },
    /// A WASI p1 command module: stdin is the argument's bytes, stdout is the representation
    /// (R-D6 point 3).
    Command { stdin: &'a [u8] },
}

/// What a convention produced.
#[derive(Debug)]
pub enum Output {
    Numeric(Vec<Value>),
    /// The postcard-encoded [`ModuleReply`](crate::session::ModuleReply).
    Session(Vec<u8>),
    Command {
        stdout: Vec<u8>,
    },
}

/// What a run consumed, for the trace (R-D10): fuel is a deterministic cost; wall time is the
/// observed duration. Either may be absent on an engine that does not meter it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Consumed {
    pub fuel: Option<u64>,
    pub wall: Option<core::time::Duration>,
}

/// A result of a run: its output and what it consumed.
#[derive(Debug)]
pub struct Invoked {
    pub output: Output,
    pub consumed: Consumed,
}

/// A compile- or load-time failure, distinct from the typed [`Error`] a run returns. A module
/// whose exports fit no convention, or that is missing a required export, fails here.
#[derive(Debug)]
pub enum EngineError {
    /// The bytes are not a valid module for this engine.
    Compile(String),
    /// The module's exports fit none of the three conventions, or a required export is missing.
    NoConvention(String),
    /// The host refused to start a run with no bound of any kind (no silent unlimited default).
    Unbounded,
}

impl core::fmt::Display for EngineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EngineError::Compile(m) => write!(f, "compile: {m}"),
            EngineError::NoConvention(m) => write!(f, "no calling convention: {m}"),
            EngineError::Unbounded => {
                f.write_str("a run needs at least one bound; there is no silent unlimited default")
            }
        }
    }
}

impl std::error::Error for EngineError {}

/// The kernel seam: a module's `host_call` resolves through this, under the SESSION's capability
/// (already clamped — a guest never gets authority by being jailed). R3 needs only `source`
/// (the fixture `hello` sources `urn:host:name`); the other verbs arrive with the full kernel
/// wiring in R3b.
///
/// Passed as an `Arc` so the engine can place it in a wasmtime `Store` (whose data must be
/// `'static`) without a lifetime trick; a real kernel is shared and `Arc`-held anyway.
pub trait HostResolver {
    /// Resolve a Source request the guest issued through `host_call`, under `capability`.
    fn source(&self, iri: &str, capability: &Capability) -> Result<Vec<u8>, Error>;
}

/// A core-module engine (R-D1). `compile` once, then `invoke` with a fresh instance per session.
pub trait CoreEngine {
    /// A compiled, cacheable module (keyed by digest and engine-compatibility hash, R-D7).
    type Compiled;

    /// Compile module bytes. The result is cached by the loader; it never comes from a
    /// precompiled file the host did not produce (R-D7).
    fn compile(&self, wasm: &[u8]) -> Result<Self::Compiled, EngineError>;

    /// Run one call under `limits`, servicing `host_call`s through `host`. A fresh instance per
    /// session means a trap costs one request, not the host.
    fn invoke(
        &self,
        compiled: &Self::Compiled,
        call: Call<'_>,
        limits: &Limits,
        host: Arc<dyn HostResolver>,
    ) -> Result<Invoked, Error>;
}
