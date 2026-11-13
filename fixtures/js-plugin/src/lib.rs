//! A Javy plugin that runs one JavaScript handler as a J2 session module (#3628, #2414).
//!
//! - `ikigai_alloc`/`ikigai_free`/`ikigai_invoke` are the ikigai-module ABI v1 exports
//!   (`ikigai_runtime_guest::ikigai_module!`). An `Invoke` calls the handler's exported
//!   `handle(target)` and answers its string as `text/plain`.
//! - The `ikigai` JavaScript surface and its codec are GENERATED from the ABI (`src/ikigai.js`,
//!   by `crates/ikigai-runtime-wasmtime/tests/js_bindings.rs`), not written here: one function
//!   per verb (`source`, `sink`, `exists`, `delete`, `meta`), each taking a target and named
//!   arguments and returning the representation. This plugin gives that script one primitive,
//!   `__ikigai_host_call(bytes)`, the raw `ikigai.host_call` import: the script encodes the
//!   `HostCall` and decodes the answer itself. Every call claims the session's own capability,
//!   and the host holds every call to the session's grant.
//! - A typed refusal surfaces in JavaScript as an exception whose `name` is the error class
//!   (`Denied`, `NotFound`, ...) and whose `message` is the resource. If the handler lets it
//!   escape unchanged, the call answers `ErrorTyped` with the same class, so the type crosses
//!   the jail.
//! - The handler is the JavaScript at `IKIGAI_JS_HANDLER` (compile time). `javy init-plugin`
//!   runs `initialize-runtime` once, ahead of time, which evaluates the bindings and compiles the
//!   handler to QuickJS bytecode: the snapshot holds both, so a call evaluates bytecode, never
//!   source.

#![allow(static_mut_refs)]

use ikigai_runtime_guest::abi::{AbiModuleCall, AbiModuleReply, AbiRepresentation};
use ikigai_runtime_guest::core_module::{dealloc, leak_parts, take_bytes, Reply};
use javy_plugin_api::javy::quickjs::{
    Context, Ctx, Error as JsError, Function, Module, TypedArray, Value,
};
use javy_plugin_api::{import_namespace, Config};

import_namespace!("ikigai_js_plugin_v1");

const HANDLER: &str = include_str!(env!("IKIGAI_JS_HANDLER"));
/// The generated `ikigai` surface and codec.
const BINDINGS: &str = include_str!("ikigai.js");

static mut CONTEXT: Option<Context> = None;
static mut BYTECODE: Option<Vec<u8>> = None;

#[link(wasm_import_module = "ikigai")]
extern "C" {
    fn host_call(ptr: u32, len: u32) -> u64;
}

/// `__ikigai_host_call(reply)`: write an encoded `ModuleReply::HostCall` to the host and return
/// its encoded answer, a `ModuleCall`. Bytes only; the generated codec reads and writes them.
fn raw_host_call<'js>(
    ctx: Ctx<'js>,
    reply: TypedArray<'js, u8>,
) -> Result<TypedArray<'js, u8>, JsError> {
    let bytes = reply.as_bytes().unwrap_or_default().to_vec();
    let (ptr, len) = leak_parts(bytes);
    // SAFETY: the buffer is live for the call; the host answers with a buffer it allocated
    // through our `ikigai_alloc`.
    let packed = unsafe { host_call(ptr, len) };
    unsafe { dealloc(ptr as *mut u8, len as usize) };
    let (rptr, rlen) = ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32);
    let answer = unsafe { take_bytes(rptr as *mut u8, rlen as usize) };
    TypedArray::new(ctx, answer)
}

fn install(ctx: &Ctx<'_>) -> Result<(), JsError> {
    ctx.globals().set(
        "__ikigai_host_call",
        Function::new(ctx.clone(), raw_host_call)?,
    )?;
    ctx.eval::<(), _>(BINDINGS)
}

#[export_name = "initialize-runtime"]
pub extern "C" fn initialize_runtime() {
    javy_plugin_api::initialize_runtime(Config::default, |runtime| {
        runtime.context().with(|ctx| install(&ctx)).unwrap();
        unsafe { CONTEXT = Some(runtime.context().clone()) };
        runtime
    })
    .unwrap();
    let bytecode = javy_plugin_api::compile_src(HANDLER.as_bytes()).unwrap();
    unsafe { BYTECODE = Some(bytecode) };
}

/// Serve one `Invoke` (its encoded bytes): `handle(target)`'s string, or the exception that
/// escaped it.
fn run(invoke: Vec<u8>) -> Reply {
    let (Some(context), Some(bytecode)) =
        (unsafe { CONTEXT.as_ref() }, unsafe { BYTECODE.as_ref() })
    else {
        return Reply::Error("the JavaScript runtime was not initialized".into());
    };
    context.with(|ctx| {
        let outcome = (|| -> Result<String, JsError> {
            let begin: Function = ctx.globals().get("__ikigai_begin")?;
            let target: String = begin.call((TypedArray::new(ctx.clone(), invoke)?,))?;
            let module = unsafe { Module::load(ctx.clone(), bytecode)? };
            let (module, promise) = module.eval()?;
            promise.finish::<()>()?;
            let handle: Function = module.get("handle")?;
            handle.call((target,))
        })();
        match outcome {
            Ok(text) => Reply::Resolved(AbiRepresentation::new("text/plain", text.into_bytes())),
            Err(JsError::Exception) => {
                let caught: Value = ctx.catch();
                let escaped: Option<Function> = ctx.globals().get("__ikigai_escaped").ok();
                let typed: Option<TypedArray<u8>> =
                    escaped.and_then(|f| f.call((caught.clone(),)).ok());
                // The kernel's own refusal escaped the handler unchanged: keep its type.
                if let Some(Ok(AbiModuleReply::ErrorTyped(e))) = typed
                    .as_ref()
                    .and_then(|t| t.as_bytes())
                    .map(AbiModuleReply::decode)
                {
                    return Reply::ErrorTyped(e);
                }
                let name: Option<String> = caught.as_object().and_then(|o| o.get("name").ok());
                let message: Option<String> =
                    caught.as_object().and_then(|o| o.get("message").ok());
                Reply::Error(format!(
                    "the handler threw {}: {}",
                    name.unwrap_or_else(|| "a value".into()),
                    message.unwrap_or_default()
                ))
            }
            Err(e) => Reply::Error(format!("the handler failed: {e}")),
        }
    })
}

fn dispatch(call: AbiModuleCall) -> Reply {
    match call {
        call @ AbiModuleCall::Invoke { .. } => match call.encode() {
            Ok(bytes) => run(bytes),
            Err(e) => Reply::Error(format!("re-encoding the call failed: {e}")),
        },
        AbiModuleCall::Describe => Reply::Bindings(None),
        _ => Reply::Error("this module answers Invoke and Describe only".into()),
    }
}

ikigai_runtime_guest::ikigai_module!(dispatch);
