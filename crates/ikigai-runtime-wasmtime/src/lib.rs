//! The wasmtime implementation of the ikigai runtime's **core-module tier** (R-D1, R-D6).
//!
//! Three calling conventions, chosen per module from its exports ([`detect_convention`]):
//! 1. **numeric signatures** directly;
//! 2. the **ikigai-module ABI v1** (`ikigai_invoke` + `host_call`, a postcard session) — a fresh
//!    `Store` per session, with `host_call` clamped to the session's capability;
//! 3. **WASI p1 command modules** (stdin → stdout), under an empty, deterministic context.
//!
//! Budgets (fuel · epoch · memory) come from [`Limits`] as the meet of the engine's host ceiling
//! and the caller's grant; the error mapping is R-D10 (fuel and memory → `OverBudget`, never
//! transient; a budget deadline → `OverBudget`, a host deadline → `Timeout`). The tier runs fully
//! **synchronously**: `host_call` resolves inline, and `Config::async_support` (a no-op in
//! wasmtime 50) is never called.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use ikigai_core::{Capability, Error};
use ikigai_runtime::session::{pack, unpack, ModuleCall, ModuleReply};
use ikigai_runtime::space::Convention;
use ikigai_runtime::{
    loader::EngineCompatHash, Call, Consumed, CoreEngine, EngineError, HostResolver, Limits,
    Output, Provenance, Value,
};
use wasmtime::{
    Caller, Config, Engine, Extern, Instance, Linker, Module, ResourceLimiter, Store, Trap, Val,
};

/// The epoch cadence: the engine's epoch is incremented every millisecond, so an epoch deadline
/// of `n` ticks is `n` milliseconds (R1 measured 10 ms ticks; 1 ms is used here for test
/// responsiveness).
const TICK: Duration = Duration::from_millis(1);

/// A memory limiter that denies growth past a ceiling and records that it did, so a trap after a
/// denied growth is reported as `OverBudget` rather than a generic endpoint failure.
struct MemLimiter {
    max_bytes: Option<usize>,
    denied: bool,
}

impl MemLimiter {
    fn new(max_bytes: Option<usize>) -> Self {
        MemLimiter {
            max_bytes,
            denied: false,
        }
    }
}

impl ResourceLimiter for MemLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmtime::Error> {
        if let Some(max) = self.max_bytes {
            if desired > max {
                self.denied = true;
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        _desired: usize,
        _maximum: Option<usize>,
    ) -> Result<bool, wasmtime::Error> {
        Ok(true)
    }
}

/// A background thread that increments the engine's epoch on a fixed cadence, so epoch deadlines
/// fire. It stops and is joined when the engine drops.
struct EpochTicker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl EpochTicker {
    fn start(engine: &Engine) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let engine = engine.clone();
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
                engine.increment_epoch();
            }
        });
        EpochTicker {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// A compiled core module, cacheable by digest and engine-compatibility hash (R-D7).
pub struct Compiled {
    module: Module,
    exports: Vec<String>,
}

impl Compiled {
    /// The module's export names, for convention detection.
    pub fn exports(&self) -> &[String] {
        &self.exports
    }
}

/// Which calling convention a module's exports select (R-D6). `ikigai_invoke` + the allocator
/// means the ABI v1 session; a `_start` export means a WASI p1 command; otherwise the caller
/// names a numeric export.
pub fn detect_convention(compiled: &Compiled) -> Convention {
    let has = |name: &str| compiled.exports.iter().any(|e| e == name);
    if has("ikigai_invoke") && has("ikigai_alloc") && has("ikigai_free") {
        Convention::Session
    } else if has("_start") {
        Convention::Command
    } else {
        Convention::Numeric
    }
}

/// The wasmtime core-module engine (R-D1).
pub struct WasmtimeCoreEngine {
    engine: Engine,
    ceiling: Limits,
    _ticker: EpochTicker,
}

impl WasmtimeCoreEngine {
    /// Build the engine with a host ceiling. **There is no silent unlimited default**: an
    /// unbounded ceiling is refused here, so the runtime "refuses to start" (R-D10, J2's rule).
    /// Every run's effective budget is `ceiling.meet(&caller_limits)`, so it is always bounded.
    pub fn new(ceiling: Limits) -> Result<Self, EngineError> {
        if !ceiling.is_bounded() {
            return Err(EngineError::Unbounded);
        }
        let mut config = Config::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        if let Some(stack) = ceiling.max_wasm_stack {
            config.max_wasm_stack(stack);
        }
        let engine = Engine::new(&config).map_err(|e| EngineError::Compile(format!("{e:#}")))?;
        let ticker = EpochTicker::start(&engine);
        Ok(WasmtimeCoreEngine {
            engine,
            ceiling,
            _ticker: ticker,
        })
    }

    /// The engine-compatibility hash a compiled form is keyed by (R-D7), as a stable string.
    pub fn compat_hash(&self) -> EngineCompatHash {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.engine
            .precompile_compatibility_hash()
            .hash(&mut hasher);
        EngineCompatHash::new(format!("wasmtime-50-{:016x}", hasher.finish()))
    }

    fn effective(&self, limits: &Limits) -> Limits {
        self.ceiling.meet(limits)
    }

    fn invoke_numeric(
        &self,
        compiled: &Compiled,
        export: &str,
        args: &[Value],
        limits: &Limits,
    ) -> Result<Invoked, Error> {
        let mut store = Store::new(&self.engine, MemLimiter::new(limits.memory_bytes));
        store.limiter(|d| d as &mut dyn ResourceLimiter);
        apply_budget(&mut store, limits);

        let instance = Instance::new(&mut store, &compiled.module, &[])
            .map_err(|e| trap_to_error(e, limits, store.data().denied))?;
        let func = instance
            .get_func(&mut store, export)
            .ok_or_else(|| Error::NotFound(format!("export `{export}`")))?;
        let params: Vec<Val> = args.iter().map(val_of).collect();
        let result_count = func.ty(&store).results().len();
        let mut results = vec![Val::I32(0); result_count];
        let before = store.get_fuel().ok();
        func.call(&mut store, &params, &mut results)
            .map_err(|e| trap_to_error(e, limits, store.data().denied))?;
        let consumed = consumed_of(&store, before);
        let out = results.iter().filter_map(value_of).collect();
        Ok(Invoked {
            output: Output::Numeric(out),
            consumed,
        })
    }

    fn invoke_session(
        &self,
        compiled: &Compiled,
        encoded_call: &[u8],
        limits: &Limits,
        host: Arc<dyn HostResolver>,
    ) -> Result<Invoked, Error> {
        let session_cap = session_cap_of(encoded_call);
        let data = SessionData {
            limiter: MemLimiter::new(limits.memory_bytes),
            host,
            session_cap,
        };
        let mut store = Store::new(&self.engine, data);
        store.limiter(|d| &mut d.limiter as &mut dyn ResourceLimiter);
        apply_budget(&mut store, limits);

        let mut linker: Linker<SessionData> = Linker::new(&self.engine);
        linker
            .func_wrap("ikigai", "host_call", host_call_pump)
            .map_err(|e| Error::Endpoint(format!("linking host_call: {e:#}")))?;
        let instance = linker
            .instantiate(&mut store, &compiled.module)
            .map_err(|e| trap_to_error(e, limits, store.data().limiter.denied))?;

        let alloc = instance
            .get_func(&mut store, "ikigai_alloc")
            .ok_or_else(|| Error::NotFound("export `ikigai_alloc`".into()))?;
        let invoke = instance
            .get_func(&mut store, "ikigai_invoke")
            .ok_or_else(|| Error::NotFound("export `ikigai_invoke`".into()))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| Error::Endpoint("guest has no `memory` export".into()))?;

        // Write the call into guest memory the host allocated through `ikigai_alloc`.
        let mut out = [Val::I32(0)];
        alloc
            .call(&mut store, &[Val::I32(encoded_call.len() as i32)], &mut out)
            .map_err(|e| trap_to_error(e, limits, store.data().limiter.denied))?;
        let ptr = as_u32(&out[0])?;
        memory
            .write(&mut store, ptr as usize, encoded_call)
            .map_err(|e| Error::Endpoint(format!("writing call into guest: {e}")))?;

        let before = store.get_fuel().ok();
        let mut ret = [Val::I64(0)];
        invoke
            .call(
                &mut store,
                &[Val::I32(ptr as i32), Val::I32(encoded_call.len() as i32)],
                &mut ret,
            )
            .map_err(|e| trap_to_error(e, limits, store.data().limiter.denied))?;
        let consumed = consumed_of(&store, before);

        let packed = match ret[0] {
            Val::I64(v) => v as u64,
            _ => return Err(Error::Endpoint("ikigai_invoke returned a non-i64".into())),
        };
        let (rptr, rlen) = unpack(packed);
        let reply = read_guest(&memory, &store, rptr, rlen)?;
        // Free the reply buffer the guest handed us (we own it).
        if let Some(free) = instance.get_func(&mut store, "ikigai_free") {
            let _ = free.call(
                &mut store,
                &[Val::I32(rptr as i32), Val::I32(rlen as i32)],
                &mut [],
            );
        }
        Ok(Invoked {
            output: Output::Session(reply),
            consumed,
        })
    }

    fn invoke_command(
        &self,
        compiled: &Compiled,
        stdin: &[u8],
        limits: &Limits,
    ) -> Result<Invoked, Error> {
        use wasmtime_wasi::p1::{add_to_linker_sync, WasiP1Ctx};
        use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
        use wasmtime_wasi::WasiCtxBuilder;

        let stdout = MemoryOutputPipe::new(1 << 20);
        // An empty, deterministic context: no preopens, env, args or sockets; stdio captured.
        let wasi: WasiP1Ctx = WasiCtxBuilder::new()
            .stdin(MemoryInputPipe::new(stdin.to_vec()))
            .stdout(stdout.clone())
            .build_p1();
        let data = P1Data {
            wasi,
            limiter: MemLimiter::new(limits.memory_bytes),
        };
        let mut store = Store::new(&self.engine, data);
        store.limiter(|d| &mut d.limiter as &mut dyn ResourceLimiter);
        apply_budget(&mut store, limits);

        let mut linker: Linker<P1Data> = Linker::new(&self.engine);
        add_to_linker_sync(&mut linker, |d: &mut P1Data| &mut d.wasi)
            .map_err(|e| Error::Endpoint(format!("linking WASI p1: {e:#}")))?;
        let instance = linker
            .instantiate(&mut store, &compiled.module)
            .map_err(|e| trap_to_error(e, limits, store.data().limiter.denied))?;
        let start = instance
            .get_func(&mut store, "_start")
            .ok_or_else(|| Error::NotFound("export `_start`".into()))?;

        let before = store.get_fuel().ok();
        match start.call(&mut store, &[], &mut []) {
            Ok(()) => {}
            Err(e) => match exit_code(&e) {
                Some(0) => {}
                Some(code) => {
                    return Err(Error::Endpoint(format!("command exited with code {code}")))
                }
                None => return Err(trap_to_error(e, limits, store.data().limiter.denied)),
            },
        }
        let consumed = consumed_of(&store, before);
        drop(store);
        let bytes = stdout.contents().to_vec();
        Ok(Invoked {
            output: Output::Command { stdout: bytes },
            consumed,
        })
    }
}

impl CoreEngine for WasmtimeCoreEngine {
    type Compiled = Compiled;

    fn compile(&self, wasm: &[u8]) -> Result<Compiled, EngineError> {
        let module =
            Module::new(&self.engine, wasm).map_err(|e| EngineError::Compile(format!("{e:#}")))?;
        let exports = module.exports().map(|e| e.name().to_string()).collect();
        Ok(Compiled { module, exports })
    }

    fn invoke(
        &self,
        compiled: &Compiled,
        call: Call<'_>,
        limits: &Limits,
        host: Arc<dyn HostResolver>,
    ) -> Result<Invoked, Error> {
        let effective = self.effective(limits);
        match call {
            Call::Numeric { export, args } => {
                self.invoke_numeric(compiled, export, args, &effective)
            }
            Call::Session { encoded_call } => {
                self.invoke_session(compiled, encoded_call, &effective, host)
            }
            Call::Command { stdin } => self.invoke_command(compiled, stdin, &effective),
        }
    }
}

// Re-export so callers name one `Invoked`.
pub use ikigai_runtime::engine::Invoked;

/// The store data for an ABI v1 session.
struct SessionData {
    limiter: MemLimiter,
    host: Arc<dyn HostResolver>,
    session_cap: Capability,
}

/// The store data for a WASI p1 command.
struct P1Data {
    wasi: wasmtime_wasi::p1::WasiP1Ctx,
    limiter: MemLimiter,
}

/// Build a `wasmtime::Error` from any `Display` message (a helper so both `&str` literals and
/// `format!` strings go through one monomorphization).
fn werr(message: impl std::fmt::Display) -> wasmtime::Error {
    wasmtime::Error::msg(message.to_string())
}

/// The `"ikigai" "host_call"` import: read the guest's `HostCall`, resolve it under the session's
/// clamped capability, and write the `HostResult` back into guest memory the guest allocates.
fn host_call_pump(
    mut caller: Caller<'_, SessionData>,
    ptr: u32,
    len: u32,
) -> Result<u64, wasmtime::Error> {
    let memory = caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .ok_or_else(|| werr("guest has no `memory` export"))?;

    let request = {
        let data = memory.data(&caller);
        let end = (ptr as usize)
            .checked_add(len as usize)
            .ok_or_else(|| werr("host_call ptr/len overflow"))?;
        data.get(ptr as usize..end)
            .ok_or_else(|| werr("host_call ptr/len out of bounds"))?
            .to_vec()
    };

    let (iri, claimed) = match postcard::from_bytes::<ModuleReply>(&request)
        .map_err(|e| werr(format!("decode HostCall: {e}")))?
    {
        ModuleReply::HostCall { iri, capability } => (iri, capability),
        other => return Err(werr(format!("expected HostCall, got {other:?}"))),
    };

    // Clamp the guest's claimed capability to the session's: a guest never widens its authority.
    let effective = caller.data().session_cap.clamp(&claimed);
    let host = Arc::clone(&caller.data().host);
    let answer = ModuleCall::HostResult(host.source(&iri, &effective).map_err(|e| e.to_string()));
    let answer_bytes =
        postcard::to_allocvec(&answer).map_err(|e| werr(format!("encode HostResult: {e}")))?;

    let alloc = caller
        .get_export("ikigai_alloc")
        .and_then(Extern::into_func)
        .ok_or_else(|| werr("guest has no `ikigai_alloc`"))?;
    let mut out = [Val::I32(0)];
    alloc.call(
        &mut caller,
        &[Val::I32(answer_bytes.len() as i32)],
        &mut out,
    )?;
    let rptr = match out[0] {
        Val::I32(p) => p as u32,
        _ => return Err(werr("ikigai_alloc returned a non-i32")),
    };
    memory.write(&mut caller, rptr as usize, &answer_bytes)?;
    Ok(pack(rptr, answer_bytes.len() as u32))
}

/// Set the store's fuel and epoch deadline from the effective limits. Fuel consumption is always
/// enabled on the engine, so a run with no fuel bound is given `u64::MAX` and governed by its
/// deadline instead (the engine ceiling guarantees at least one real bound).
fn apply_budget<T>(store: &mut Store<T>, limits: &Limits) {
    let _ = store.set_fuel(limits.fuel.unwrap_or(u64::MAX));
    // Epoch interruption is enabled engine-wide, so a store that sets NO deadline would otherwise
    // trap immediately (its default deadline is the current epoch). Set it far out when there is
    // no deadline, so only fuel governs the run.
    let ticks = match limits.deadline {
        Some(deadline) => (deadline.after.as_millis() as u64).max(1),
        None => u64::MAX,
    };
    store.set_epoch_deadline(ticks);
}

fn consumed_of<T>(store: &Store<T>, before: Option<u64>) -> Consumed {
    let after = store.get_fuel().ok();
    let fuel = match (before, after) {
        (Some(b), Some(a)) => Some(b.saturating_sub(a)),
        _ => None,
    };
    Consumed { fuel, wall: None }
}

/// Map a run failure to a typed error (R-D10). A denied memory growth is `OverBudget`; otherwise
/// a fuel trap is `OverBudget`, an epoch trap is `OverBudget` for a budget deadline and `Timeout`
/// for a host one, and any other trap is `Endpoint`.
fn trap_to_error(err: wasmtime::Error, limits: &Limits, memory_denied: bool) -> Error {
    if memory_denied {
        return Error::OverBudget {
            detail: "module exceeded its memory budget".to_string(),
        };
    }
    if let Some(trap) = err.downcast_ref::<Trap>() {
        return match trap {
            Trap::OutOfFuel => Error::OverBudget {
                detail: format!("module ran out of fuel (budget {:?})", limits.fuel),
            },
            Trap::Interrupt => match limits.deadline.map(|d| d.provenance) {
                Some(Provenance::Budget) => Error::OverBudget {
                    detail: "module ran past its time budget".to_string(),
                },
                _ => Error::Timeout("module ran past its deadline".to_string()),
            },
            other => Error::Endpoint(format!("module trapped: {other}")),
        };
    }
    Error::Endpoint(format!("module failed: {err:#}"))
}

/// The session's capability is the one carried on its `Invoke`. A malformed call defaults to no
/// authority (deny-all), never to the host's.
fn session_cap_of(encoded: &[u8]) -> Capability {
    match postcard::from_bytes::<ModuleCall>(encoded) {
        Ok(ModuleCall::Invoke { capability, .. }) => capability,
        _ => Capability::root().attenuate(std::iter::empty::<String>()),
    }
}

fn read_guest<T>(
    memory: &wasmtime::Memory,
    store: &Store<T>,
    ptr: u32,
    len: u32,
) -> Result<Vec<u8>, Error> {
    let data = memory.data(store);
    let end = (ptr as usize)
        .checked_add(len as usize)
        .ok_or_else(|| Error::Endpoint("guest reply ptr/len overflow".into()))?;
    data.get(ptr as usize..end)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| Error::Endpoint("guest reply ptr/len out of bounds".into()))
}

fn val_of(value: &Value) -> Val {
    match value {
        Value::I32(v) => Val::I32(*v),
        Value::I64(v) => Val::I64(*v),
        Value::F32(v) => Val::F32(v.to_bits()),
        Value::F64(v) => Val::F64(v.to_bits()),
    }
}

fn value_of(val: &Val) -> Option<Value> {
    match val {
        Val::I32(v) => Some(Value::I32(*v)),
        Val::I64(v) => Some(Value::I64(*v)),
        Val::F32(bits) => Some(Value::F32(f32::from_bits(*bits))),
        Val::F64(bits) => Some(Value::F64(f64::from_bits(*bits))),
        _ => None,
    }
}

fn as_u32(val: &Val) -> Result<u32, Error> {
    match val {
        Val::I32(v) => Ok(*v as u32),
        _ => Err(Error::Endpoint("expected an i32 from the guest".into())),
    }
}

/// The exit code of a WASI `proc_exit`, if the failure was one (a clean `exit(0)` is success).
fn exit_code(err: &wasmtime::Error) -> Option<i32> {
    err.downcast_ref::<wasmtime_wasi::I32Exit>().map(|e| e.0)
}
