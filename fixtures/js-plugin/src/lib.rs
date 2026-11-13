//! A Javy plugin that runs one JavaScript handler as a J2 session module (#3628).
//!
//! - `ikigai_alloc`/`ikigai_free`/`ikigai_invoke` are the ikigai-module ABI v1 exports
//!   (`ikigai_runtime_guest::ikigai_module!`). An `Invoke` calls the handler's exported
//!   `handle(target)` and answers its string as `text/plain`.
//! - JavaScript reaches the kernel only through the global `ikigai` object, whose verbs make an
//!   `ikigai.host_call` claiming the session's own capability: `ikigai.source(iri)` returns the
//!   representation's text, `ikigai.sink(iri, text)` writes `text` as the `body` argument. The
//!   host holds every call to the session's grant.
//! - A typed refusal surfaces in JavaScript as an exception whose `name` is the error class
//!   (`Denied`, `NotFound`, ...) and whose `message` is the resource. If the handler lets it
//!   escape, the call answers `ErrorTyped` with the same class, so the type crosses the jail.
//! - The handler is the JavaScript at `IKIGAI_JS_HANDLER` (compile time). `javy init-plugin`
//!   runs `initialize-runtime` once, ahead of time, which also compiles it to QuickJS bytecode:
//!   the snapshot holds the runtime and the bytecode, so a call evaluates bytecode, never source.

#![allow(static_mut_refs)]

use ikigai_runtime_guest::abi::{
    AbiArgRef, AbiCapability, AbiModuleCall, AbiModuleError, AbiRepresentation, AbiRequest, AbiVerb,
};
use ikigai_runtime_guest::core_module::{host_call, Reply};
use javy_plugin_api::javy::quickjs::{
    prelude::Func, Context, Ctx, Error as JsError, Exception, Function, Module, Object, Value,
};
use javy_plugin_api::{import_namespace, Config};

import_namespace!("ikigai_js_plugin_v1");

const HANDLER: &str = include_str!(env!("IKIGAI_JS_HANDLER"));

static mut CONTEXT: Option<Context> = None;
static mut BYTECODE: Option<Vec<u8>> = None;
/// The capability of the session being served: every host call claims it, never more.
static mut SESSION: Option<AbiCapability> = None;
/// The last typed refusal thrown into JavaScript, matched against an escaping exception.
static mut THROWN: Option<AbiModuleError> = None;

fn class(e: &AbiModuleError) -> (&'static str, String) {
    match e {
        AbiModuleError::Unresolved(s) => ("Unresolved", s.clone()),
        AbiModuleError::MissingArgument(s) => ("MissingArgument", s.clone()),
        AbiModuleError::InvalidArgument { name, detail } => {
            ("InvalidArgument", format!("{name}: {detail}"))
        }
        AbiModuleError::Endpoint(s) => ("Endpoint", s.clone()),
        AbiModuleError::Denied(s) => ("Denied", s.clone()),
        AbiModuleError::NotFound(s) => ("NotFound", s.clone()),
        AbiModuleError::Timeout(s) => ("Timeout", s.clone()),
        AbiModuleError::Unavailable(s) => ("Unavailable", s.clone()),
    }
}

/// Make a host call and turn a typed failure into a JavaScript exception naming its class.
fn kernel<'js>(ctx: &Ctx<'js>, request: AbiRequest) -> Result<AbiRepresentation, JsError> {
    let claim = unsafe { SESSION.clone() }.unwrap_or_else(|| AbiCapability::scoped([""; 0]));
    match host_call(&request, &claim) {
        Ok(r) => Ok(r),
        Err(e) => {
            let (name, message) = class(&e);
            unsafe { THROWN = Some(e) };
            let exception = Exception::from_message(ctx.clone(), &message)?;
            exception.as_object().set("name", name)?;
            Err(ctx.throw(exception.into_value()))
        }
    }
}

fn install(ctx: &Ctx<'_>) -> Result<(), JsError> {
    let ikigai = Object::new(ctx.clone())?;
    ikigai.set(
        "source",
        Func::from(|ctx: Ctx<'_>, iri: String| -> Result<String, JsError> {
            let r = kernel(&ctx, AbiRequest::new(AbiVerb::Source, iri))?;
            Ok(String::from_utf8_lossy(&r.bytes).into_owned())
        }),
    )?;
    ikigai.set(
        "sink",
        Func::from(
            |ctx: Ctx<'_>, iri: String, text: String| -> Result<(), JsError> {
                let request = AbiRequest::new(AbiVerb::Sink, iri)
                    .with_arg("body", AbiArgRef::Inline(text.into_bytes()));
                kernel(&ctx, request).map(|_| ())
            },
        ),
    )?;
    ctx.globals().set("ikigai", ikigai)
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

/// Run `handle(target)`: its string, or the exception that escaped it.
fn run(target: &str) -> Reply {
    let (Some(context), Some(bytecode)) =
        (unsafe { CONTEXT.as_ref() }, unsafe { BYTECODE.as_ref() })
    else {
        return Reply::Error("the JavaScript runtime was not initialized".into());
    };
    context.with(|ctx| {
        let outcome = (|| -> Result<String, JsError> {
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
                let name: Option<String> = caught.as_object().and_then(|o| o.get("name").ok());
                let message: Option<String> =
                    caught.as_object().and_then(|o| o.get("message").ok());
                match unsafe { THROWN.take() } {
                    // The kernel's own refusal escaped the handler unchanged: keep its type.
                    Some(e)
                        if name.as_deref() == Some(class(&e).0)
                            && message.as_deref() == Some(class(&e).1.as_str()) =>
                    {
                        Reply::ErrorTyped(e)
                    }
                    _ => Reply::Error(format!(
                        "the handler threw {}: {}",
                        name.unwrap_or_else(|| "a value".into()),
                        message.unwrap_or_default()
                    )),
                }
            }
            Err(e) => Reply::Error(format!("the handler failed: {e}")),
        }
    })
}

fn dispatch(call: AbiModuleCall) -> Reply {
    match call {
        AbiModuleCall::Invoke {
            request,
            capability,
        } => {
            unsafe {
                SESSION = Some(capability);
                THROWN = None;
            }
            run(&request.target)
        }
        AbiModuleCall::Describe => Reply::Bindings(None),
        _ => Reply::Error("this module answers Invoke and Describe only".into()),
    }
}

ikigai_runtime_guest::ikigai_module!(dispatch);
