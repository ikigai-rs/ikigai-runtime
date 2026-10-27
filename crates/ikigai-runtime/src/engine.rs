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
use core::future::Future;
use core::pin::Pin;
use ikigai_core::{Capability, Error, Kernel, Representation, Request};
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
        /// The postcard-encoded `ModuleCall` (an `Invoke`, `Describe`, `Manifest` or `Cards`;
        /// see [`session`](crate::session)). The host wrote it, so the capability on an `Invoke`
        /// is the session's authority; every `host_call` the guest makes is held to it.
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
    /// The postcard-encoded final `ModuleReply` (see [`session`](crate::session)).
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

/// A boxed, `Send` future borrowing for `'a`: what a [`HostResolver`] returns.
pub type HostFuture<'a> = Pin<Box<dyn Future<Output = Result<Representation, Error>> + Send + 'a>>;

/// The kernel seam: every request a guest makes of the kernel (a core module's `host_call`, a
/// component's `kernel` import, a configured import binding, `wasi:http`) resolves through this,
/// under a capability **the host** chose: the session's, clamped (a guest never gets authority by
/// being jailed, ledger #83). All five verbs arrive as a typed [`Request`]; the answer is the
/// kernel's own [`Representation`] or typed [`Error`].
///
/// Async, because the real kernel is: [`KernelResolver`] issues through
/// [`Kernel::issue`](ikigai_core::Kernel::issue), and a host that runs a module INSIDE an
/// invocation bridges to `Invocation::issue` instead, so each guest call is a tracked dependency
/// (`ikigai-runtime-wasmtime`'s `JailTransport` does that). Passed as an `Arc` so the engine can
/// keep it in a wasmtime `Store`, whose data must be `'static`.
pub trait HostResolver: Send + Sync {
    /// Resolve `request` under `capability`.
    fn resolve<'a>(&'a self, request: Request, capability: &'a Capability) -> HostFuture<'a>;
}

/// A [`HostResolver`] over a whole kernel: each guest request is a top-level
/// [`Kernel::issue`](ikigai_core::Kernel::issue) under the session's capability. For a host that
/// runs a module on its own behalf (a test, a shell, a scheduled job). A module run inside
/// another resource's invocation bridges to that invocation instead, so the guest's reads are
/// dependencies of the answer.
#[derive(Clone)]
pub struct KernelResolver {
    kernel: Arc<Kernel>,
}

impl KernelResolver {
    /// Resolve guest requests through `kernel`.
    pub fn new(kernel: Arc<Kernel>) -> Self {
        KernelResolver { kernel }
    }
}

impl HostResolver for KernelResolver {
    fn resolve<'a>(&'a self, request: Request, capability: &'a Capability) -> HostFuture<'a> {
        Box::pin(self.kernel.issue(request, capability))
    }
}

/// A [`HostResolver`] that refuses every request, as [`Error::Denied`]: for a run that must make
/// no kernel calls (a mount-time `Describe`, a numeric or command module).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoHost;

impl HostResolver for NoHost {
    fn resolve<'a>(&'a self, request: Request, _capability: &'a Capability) -> HostFuture<'a> {
        let target = request.target.as_str().to_string();
        Box::pin(async move {
            Err(Error::Denied(format!(
                "{target}: this run has no kernel to call"
            )))
        })
    }
}

/// A core-module engine (R-D1). `compile` once, then `invoke` with a fresh instance per session.
pub trait CoreEngine {
    /// A compiled, cacheable module (keyed by digest and engine-compatibility hash, R-D7).
    type Compiled;

    /// Compile module bytes. The result is cached by the loader; it never comes from a
    /// precompiled file the host did not produce (R-D7).
    fn compile(&self, wasm: &[u8]) -> Result<Self::Compiled, EngineError>;

    /// Run one call under `limits`, servicing `host_call`s through `host`. A fresh instance per
    /// session means a trap costs one request, not the host. Async because a `host_call` waits
    /// on the kernel; the guest itself still sees a synchronous call.
    fn invoke(
        &self,
        compiled: &Self::Compiled,
        call: Call<'_>,
        limits: &Limits,
        host: Arc<dyn HostResolver>,
    ) -> impl Future<Output = Result<Invoked, Error>> + Send;
}
