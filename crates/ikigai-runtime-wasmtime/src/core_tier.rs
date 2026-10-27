//! The **core-module tier** on wasmtime (R-D1, R-D6): three calling conventions, a fresh `Store`
//! per session, budgets from [`Limits`], and every `host_call` held to the session's capability.

use std::sync::Arc;
use std::time::Instant;

use ikigai_core::{Capability, Error};
use ikigai_module::{ModuleCall, ModuleError, ModuleReply};
use ikigai_runtime::session::{
    abi::AbiModuleReply, pack, unpack, EXPORT_ALLOC, EXPORT_FREE, EXPORT_INVOKE, EXPORT_MEMORY,
    IMPORT_HOST_CALL, IMPORT_MODULE,
};
use ikigai_runtime::space::Convention;
use ikigai_runtime::{
    loader::{ArtifactDigest, EngineCompatHash},
    Call, CoreEngine, EngineError, HostResolver, Invoked, Limits, Output, Value,
};
use wasmtime::{Caller, Engine, Extern, Instance, Linker, Module, ResourceLimiter, Store, Val};

use crate::budget::{
    apply_budget, check_metering, consumed_of, engine_for, trap_to_error, werr, EpochTicker,
    MemLimiter, Metering,
};

/// A compiled core module, cacheable by digest and engine-compatibility hash (R-D7).
pub struct Compiled {
    module: Module,
    exports: Vec<String>,
    digest: ArtifactDigest,
}

impl Compiled {
    /// The module's export names, for convention detection.
    pub fn exports(&self) -> &[String] {
        &self.exports
    }

    /// The digest of the bytes it was compiled from (the artifact's name, R-D7).
    pub fn digest(&self) -> &ArtifactDigest {
        &self.digest
    }
}

/// Which calling convention a module's exports select (R-D6). `ikigai_invoke` + the allocator
/// means the ABI v1 session; a `_start` export means a WASI p1 command; otherwise the caller
/// names a numeric export.
pub fn detect_convention(compiled: &Compiled) -> Convention {
    let has = |name: &str| compiled.exports.iter().any(|e| e == name);
    if has(EXPORT_INVOKE) && has(EXPORT_ALLOC) && has(EXPORT_FREE) {
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
    metering: Metering,
    _ticker: EpochTicker,
}

impl WasmtimeCoreEngine {
    /// Build the engine with a host ceiling, metering by fuel and epoch. **There is no silent
    /// unlimited default**: an unbounded ceiling is refused here, so the runtime "refuses to
    /// start" (R-D10, J2's rule). Every run's effective budget is `ceiling.meet(&caller_limits)`,
    /// so it is always bounded.
    pub fn new(ceiling: Limits) -> Result<Self, EngineError> {
        Self::with_metering(ceiling, Metering::FuelAndEpoch)
    }

    /// Build the engine with an explicit [`Metering`]: an epoch-only engine skips fuel's cost
    /// and refuses any run that asks for a fuel budget.
    pub fn with_metering(ceiling: Limits, metering: Metering) -> Result<Self, EngineError> {
        let engine = engine_for(&ceiling, metering)?;
        let ticker = EpochTicker::start(&engine);
        Ok(WasmtimeCoreEngine {
            engine,
            ceiling,
            metering,
            _ticker: ticker,
        })
    }

    /// The engine-compatibility hash a compiled form is keyed by (R-D7), as a stable string.
    pub fn compat_hash(&self) -> EngineCompatHash {
        crate::compat_hash(&self.engine)
    }

    fn effective(&self, limits: &Limits) -> Limits {
        self.ceiling.meet(limits)
    }

    async fn invoke_numeric(
        &self,
        compiled: &Compiled,
        export: &str,
        args: &[Value],
        limits: &Limits,
    ) -> Result<Invoked, Error> {
        let mut store = Store::new(&self.engine, MemLimiter::new(limits.memory_bytes));
        store.limiter(|d| d as &mut dyn ResourceLimiter);
        apply_budget(&mut store, limits, self.metering);

        let instance = Instance::new_async(&mut store, &compiled.module, &[])
            .await
            .map_err(|e| trap_to_error(e, limits, store.data().denied))?;
        let func = instance
            .get_func(&mut store, export)
            .ok_or_else(|| Error::NotFound(format!("export `{export}`")))?;
        let params: Vec<Val> = args.iter().map(val_of).collect();
        let result_count = func.ty(&store).results().len();
        let mut results = vec![Val::I32(0); result_count];
        let before = store.get_fuel().ok();
        let started = Instant::now();
        func.call_async(&mut store, &params, &mut results)
            .await
            .map_err(|e| trap_to_error(e, limits, store.data().denied))?;
        let mut consumed = consumed_of(&store, before);
        consumed.wall = Some(started.elapsed());
        let out = results.iter().filter_map(value_of).collect();
        Ok(Invoked {
            output: Output::Numeric(out),
            consumed,
        })
    }

    async fn invoke_session(
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
        apply_budget(&mut store, limits, self.metering);

        let mut linker: Linker<SessionData> = Linker::new(&self.engine);
        linker
            .func_wrap_async(
                IMPORT_MODULE,
                IMPORT_HOST_CALL,
                |caller: Caller<'_, SessionData>, (ptr, len): (u32, u32)| {
                    Box::new(host_call_pump(caller, ptr, len))
                },
            )
            .map_err(|e| Error::Endpoint(format!("linking host_call: {e:#}")))?;
        let instance = linker
            .instantiate_async(&mut store, &compiled.module)
            .await
            .map_err(|e| trap_to_error(e, limits, store.data().limiter.denied))?;

        let alloc = instance
            .get_func(&mut store, EXPORT_ALLOC)
            .ok_or_else(|| Error::NotFound(format!("export `{EXPORT_ALLOC}`")))?;
        let invoke = instance
            .get_func(&mut store, EXPORT_INVOKE)
            .ok_or_else(|| Error::NotFound(format!("export `{EXPORT_INVOKE}`")))?;
        let memory = instance
            .get_memory(&mut store, EXPORT_MEMORY)
            .ok_or_else(|| Error::Endpoint(format!("guest has no `{EXPORT_MEMORY}` export")))?;

        // Write the call into guest memory the host allocated through `ikigai_alloc`.
        let mut out = [Val::I32(0)];
        alloc
            .call_async(&mut store, &[Val::I32(encoded_call.len() as i32)], &mut out)
            .await
            .map_err(|e| trap_to_error(e, limits, store.data().limiter.denied))?;
        let ptr = as_u32(&out[0])?;
        memory
            .write(&mut store, ptr as usize, encoded_call)
            .map_err(|e| Error::Endpoint(format!("writing call into guest: {e}")))?;

        let before = store.get_fuel().ok();
        let started = Instant::now();
        let mut ret = [Val::I64(0)];
        invoke
            .call_async(
                &mut store,
                &[Val::I32(ptr as i32), Val::I32(encoded_call.len() as i32)],
                &mut ret,
            )
            .await
            .map_err(|e| trap_to_error(e, limits, store.data().limiter.denied))?;
        let mut consumed = consumed_of(&store, before);
        consumed.wall = Some(started.elapsed());

        let packed = match ret[0] {
            Val::I64(v) => v as u64,
            _ => {
                return Err(Error::Endpoint(format!(
                    "{EXPORT_INVOKE} returned a non-i64"
                )))
            }
        };
        let (rptr, rlen) = unpack(packed);
        let reply = read_guest(&memory, &store, rptr, rlen)?;
        // Free the reply buffer the guest handed us (we own it).
        if let Some(free) = instance.get_func(&mut store, EXPORT_FREE) {
            let _ = free
                .call_async(
                    &mut store,
                    &[Val::I32(rptr as i32), Val::I32(rlen as i32)],
                    &mut [],
                )
                .await;
        }
        Ok(Invoked {
            output: Output::Session(reply),
            consumed,
        })
    }

    async fn invoke_command(
        &self,
        compiled: &Compiled,
        stdin: &[u8],
        limits: &Limits,
    ) -> Result<Invoked, Error> {
        use wasmtime_wasi::p1::{add_to_linker_async, WasiP1Ctx};
        use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};

        let stdout = MemoryOutputPipe::new(crate::wasi::STDIO_CAPACITY);
        // The empty, deterministic context (R-D6 point 3): no preopens, env, args or sockets; a
        // fixed clock and a seeded RNG; stdio captured.
        let mut builder = crate::wasi::deterministic_builder(&crate::wasi::seed_of(
            compiled.digest.as_str().as_bytes(),
            stdin,
        ));
        builder
            .stdin(MemoryInputPipe::new(stdin.to_vec()))
            .stdout(stdout.clone());
        let wasi: WasiP1Ctx = builder.build_p1();
        let data = P1Data {
            wasi,
            limiter: MemLimiter::new(limits.memory_bytes),
        };
        let mut store = Store::new(&self.engine, data);
        store.limiter(|d| &mut d.limiter as &mut dyn ResourceLimiter);
        apply_budget(&mut store, limits, self.metering);

        let mut linker: Linker<P1Data> = Linker::new(&self.engine);
        add_to_linker_async(&mut linker, |d: &mut P1Data| &mut d.wasi)
            .map_err(|e| Error::Endpoint(format!("linking WASI p1: {e:#}")))?;
        let instance = linker
            .instantiate_async(&mut store, &compiled.module)
            .await
            .map_err(|e| trap_to_error(e, limits, store.data().limiter.denied))?;
        let start = instance
            .get_func(&mut store, "_start")
            .ok_or_else(|| Error::NotFound("export `_start`".into()))?;

        let before = store.get_fuel().ok();
        let started = Instant::now();
        match start.call_async(&mut store, &[], &mut []).await {
            Ok(()) => {}
            Err(e) => match exit_code(&e) {
                Some(0) => {}
                Some(code) => {
                    return Err(Error::Endpoint(format!("command exited with code {code}")))
                }
                None => return Err(trap_to_error(e, limits, store.data().limiter.denied)),
            },
        }
        let mut consumed = consumed_of(&store, before);
        consumed.wall = Some(started.elapsed());
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
        Ok(Compiled {
            module,
            exports,
            digest: ArtifactDigest::of(wasm),
        })
    }

    async fn invoke(
        &self,
        compiled: &Compiled,
        call: Call<'_>,
        limits: &Limits,
        host: Arc<dyn HostResolver>,
    ) -> Result<Invoked, Error> {
        check_metering(limits, self.metering)?;
        let effective = self.effective(limits);
        match call {
            Call::Numeric { export, args } => {
                self.invoke_numeric(compiled, export, args, &effective)
                    .await
            }
            Call::Session { encoded_call } => {
                self.invoke_session(compiled, encoded_call, &effective, host)
                    .await
            }
            Call::Command { stdin } => self.invoke_command(compiled, stdin, &effective).await,
        }
    }
}

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

/// The `"ikigai" "host_call"` import: read the guest's `HostCall`, resolve it under the session's
/// capability, and write the answer back into guest memory the guest allocates.
///
/// The guest's bytes are decoded as the wire-only mirror and converted to ikigai-module's typed
/// session at this boundary (R-D6 as amended): a target that is not an IRI is answered as a typed
/// `InvalidArgument`, never trusted. **The capability the guest carries is a claim**: it is
/// clamped to the session's, so a guest narrows its authority and never widens it (ledger #83).
async fn host_call_pump(
    mut caller: Caller<'_, SessionData>,
    ptr: u32,
    len: u32,
) -> Result<u64, wasmtime::Error> {
    let memory = caller
        .get_export(EXPORT_MEMORY)
        .and_then(Extern::into_memory)
        .ok_or_else(|| werr(format!("guest has no `{EXPORT_MEMORY}` export")))?;

    let request_bytes = {
        let data = memory.data(&caller);
        let end = (ptr as usize)
            .checked_add(len as usize)
            .ok_or_else(|| werr("host_call ptr/len overflow"))?;
        data.get(ptr as usize..end)
            .ok_or_else(|| werr("host_call ptr/len out of bounds"))?
            .to_vec()
    };

    let answer = match AbiModuleReply::decode(&request_bytes) {
        Ok(abi) => match ModuleReply::try_from(abi) {
            Ok(ModuleReply::HostCall {
                request,
                capability: claimed,
            }) => {
                let effective = caller.data().session_cap.clamp(&claimed);
                let host = Arc::clone(&caller.data().host);
                host_answer(host.resolve(request, &effective).await)
            }
            Ok(_) => ModuleCall::HostResult(Err("host_call: expected a HostCall".to_string())),
            Err(e) => ModuleCall::HostError(ModuleError::InvalidArgument {
                name: "target".to_string(),
                detail: e.detail().to_string(),
            }),
        },
        Err(e) => ModuleCall::HostResult(Err(format!("host_call: undecodable HostCall: {e}"))),
    };
    let answer_bytes =
        postcard::to_allocvec(&answer).map_err(|e| werr(format!("encode host answer: {e}")))?;

    let alloc = caller
        .get_export(EXPORT_ALLOC)
        .and_then(Extern::into_func)
        .ok_or_else(|| werr(format!("guest has no `{EXPORT_ALLOC}`")))?;
    let mut out = [Val::I32(0)];
    alloc
        .call_async(
            &mut caller,
            &[Val::I32(answer_bytes.len() as i32)],
            &mut out,
        )
        .await?;
    let rptr = match out[0] {
        Val::I32(p) => p as u32,
        _ => return Err(werr(format!("{EXPORT_ALLOC} returned a non-i32"))),
    };
    memory.write(&mut caller, rptr as usize, &answer_bytes)?;
    Ok(pack(rptr, answer_bytes.len() as u32))
}

/// The host's answer to one `HostCall`: `HostResult` whenever it is lossless, `HostError` for a
/// failure whose type a bare string would lose.
///
/// This is ikigai-module's private `host_answer` rule, restated because the crate does not
/// export it (reported to the hub: a third transport should not have to copy the session's
/// rules). `tests/jail.rs` pins the typed-error half across the jail.
pub(crate) fn host_answer(result: Result<ikigai_core::Representation, Error>) -> ModuleCall {
    match result {
        Ok(representation) => ModuleCall::HostResult(Ok(representation)),
        Err(Error::Endpoint(message)) => ModuleCall::HostResult(Err(message)),
        Err(other) => ModuleCall::HostError(ModuleError::from(&other)),
    }
}

/// The session's capability is the one carried on its `Invoke`: the host wrote that call, so it
/// is the host's statement of authority. Anything else (a `Describe`, a `Manifest`, a `Cards`, a
/// malformed call) runs with no authority at all (deny-all), never the host's.
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
