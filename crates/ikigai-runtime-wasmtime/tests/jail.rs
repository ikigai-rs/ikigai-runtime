//! The ikigai-module ABI v1 (J2) session tier, end to end through the `jail-guest` fixture
//! (R-D6, R-D18), on ikigai-module's own session (R-D6 as amended): directly through the engine,
//! and mounted into a kernel through ikigai-module's `ModuleSpace` over [`JailTransport`]. Behind
//! the `jail-fixtures` feature, which the build script and the dedicated CI job turn on with the
//! wasm targets installed.
#![cfg(feature = "jail-fixtures")]

use std::sync::Arc;
use std::time::Duration;

use ikigai_core::{
    Description, EndpointSpace, Exact, Fallback, FnEndpoint, Invocation, Iri, Kernel,
    MetaRenderer, ReprType, Request, Space, Verb as CoreVerb,
};
use ikigai_module::{ModuleCall, ModuleFloor, ModuleReply, ModuleSpace};
use ikigai_runtime::space::Convention;
use ikigai_runtime::{
    Call, Capability, CoreEngine, Deadline, Error, HostFuture, HostResolver, Limits, Output,
    Representation,
};
use ikigai_runtime_wasmtime::{detect_convention, Compiled, JailTransport, WasmtimeCoreEngine};

/// The guest wasm, built by `build.rs` under the `jail-fixtures` feature.
const JAIL_GUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/jail_guest.wasm"));

/// The scope the host name requires.
const READ_NAME: &str = "urn:cap:host:read:name";

/// A host that serves `urn:host:name`, but only to a capability that allows reading it — so the
/// capability floor is observable across the jail.
struct HostName;

impl HostResolver for HostName {
    fn resolve<'a>(&'a self, request: Request, capability: &'a Capability) -> HostFuture<'a> {
        Box::pin(async move {
            if request.target.as_str() != "urn:host:name" {
                return Err(Error::NotFound(request.target.as_str().to_string()));
            }
            if !capability.allows(READ_NAME) {
                return Err(Error::Denied("urn:host:name".to_string()));
            }
            Ok(Representation::new(ReprType::new("text/plain"), b"camel".to_vec()))
        })
    }
}

fn engine() -> WasmtimeCoreEngine {
    WasmtimeCoreEngine::new(Limits::fuel(u64::MAX)).expect("a bounded ceiling")
}

fn compiled(engine: &WasmtimeCoreEngine) -> Compiled {
    engine.compile(JAIL_GUEST).expect("the guest compiles")
}

/// A capability that allows reading the host name.
fn allowing() -> Capability {
    Capability::root().attenuate([READ_NAME.to_string()])
}

/// A capability that allows nothing.
fn empty() -> Capability {
    Capability::root().attenuate(std::iter::empty::<String>())
}

fn source(target: &str) -> Request {
    Request::new(CoreVerb::Source, Iri::parse(target).unwrap())
}

/// Encode an `Invoke`, run it through the session convention, and decode the typed reply.
async fn invoke(
    engine: &WasmtimeCoreEngine,
    module: &Compiled,
    endpoint: &str,
    capability: Capability,
    limits: &Limits,
) -> Result<(ModuleReply, Option<u64>), Error> {
    let call = ModuleCall::Invoke {
        request: source(endpoint),
        capability,
    };
    let encoded = postcard::to_allocvec(&call).unwrap();
    let invoked = engine
        .invoke(
            module,
            Call::Session {
                encoded_call: &encoded,
            },
            limits,
            Arc::new(HostName),
        )
        .await?;
    let bytes = match invoked.output {
        Output::Session(bytes) => bytes,
        _ => panic!("expected a session output"),
    };
    Ok((postcard::from_bytes(&bytes).unwrap(), invoked.consumed.fuel))
}

fn resolved_bytes(reply: ModuleReply) -> Vec<u8> {
    match reply {
        ModuleReply::Resolved(representation) => representation.bytes,
        other => panic!("expected Resolved, got {other:?}"),
    }
}

#[tokio::test]
async fn the_guest_detects_as_the_session_convention() {
    let engine = engine();
    assert_eq!(detect_convention(&compiled(&engine)), Convention::Session);
}

#[tokio::test]
async fn hello_sources_the_host_name_through_a_host_call() {
    let engine = engine();
    let module = compiled(&engine);
    let (reply, _) = invoke(
        &engine,
        &module,
        "urn:jail:hello",
        allowing(),
        &Limits::fuel(50_000_000),
    )
    .await
    .unwrap();
    assert_eq!(resolved_bytes(reply), b"hello, camel");
}

#[tokio::test]
async fn a_refusal_keeps_its_type_across_the_jail() {
    let engine = engine();
    let module = compiled(&engine);
    // The host answers `HostError(Denied)`, the guest passes it back as `ErrorTyped`: the type
    // survives both crossings, so a caller sees a permanent denial, never a retryable string.
    let (reply, _) = invoke(
        &engine,
        &module,
        "urn:jail:hello",
        empty(),
        &Limits::fuel(50_000_000),
    )
    .await
    .unwrap();
    match reply {
        ModuleReply::ErrorTyped(error) => {
            assert!(matches!(Error::from(error), Error::Denied(_)))
        }
        other => panic!("expected a typed denial, got {other:?}"),
    }
}

#[tokio::test]
async fn the_capability_floor_holds_across_the_jail() {
    let engine = engine();
    let module = compiled(&engine);
    // `escalate` claims root on its host_call, but the session grants nothing: the host clamps it,
    // so the read is denied — a guest cannot widen its authority by being jailed (ledger #83).
    let (reply, _) = invoke(
        &engine,
        &module,
        "urn:jail:escalate",
        empty(),
        &Limits::fuel(50_000_000),
    )
    .await
    .unwrap();
    match reply {
        ModuleReply::ErrorTyped(error) => match Error::from(error) {
            Error::Denied(message) => assert!(message.contains("urn:host:name"), "{message}"),
            other => panic!("expected Denied, got {other:?}"),
        },
        other => panic!("expected a denied error, got {other:?}"),
    }
    // With a session that DOES grant it, the same guest (claiming root) succeeds — the clamp
    // allows exactly what the session already holds.
    let (ok, _) = invoke(
        &engine,
        &module,
        "urn:jail:escalate",
        allowing(),
        &Limits::fuel(50_000_000),
    )
    .await
    .unwrap();
    assert_eq!(resolved_bytes(ok), b"camel");
}

#[tokio::test]
async fn describe_returns_the_bound_entries() {
    let engine = engine();
    let module = compiled(&engine);
    let call = postcard::to_allocvec(&ModuleCall::Describe).unwrap();
    let invoked = engine
        .invoke(
            &module,
            Call::Session {
                encoded_call: &call,
            },
            &Limits::fuel(50_000_000),
            Arc::new(HostName),
        )
        .await
        .unwrap();
    let Output::Session(bytes) = invoked.output else {
        panic!("expected a session output");
    };
    match postcard::from_bytes::<ModuleReply>(&bytes).unwrap() {
        ModuleReply::Bindings(Some(entries)) => assert!(
            entries.iter().any(|e| e.pattern == "urn:jail:hello"),
            "got {entries:?}"
        ),
        other => panic!("expected Bindings, got {other:?}"),
    }
}

#[tokio::test]
async fn fuel_consumed_by_hello_is_identical_across_two_runs() {
    let engine = engine();
    let module = compiled(&engine);
    let limits = Limits::fuel(50_000_000);
    let (_, a) = invoke(&engine, &module, "urn:jail:hello", allowing(), &limits)
        .await
        .unwrap();
    let (_, b) = invoke(&engine, &module, "urn:jail:hello", allowing(), &limits)
        .await
        .unwrap();
    assert!(a.is_some());
    assert_eq!(a, b, "fuel is a deterministic cost");
}

async fn host_survives(engine: &WasmtimeCoreEngine, module: &Compiled) {
    let (ok, _) = invoke(
        engine,
        module,
        "urn:jail:hello",
        allowing(),
        &Limits::fuel(50_000_000),
    )
    .await
    .unwrap();
    assert_eq!(resolved_bytes(ok), b"hello, camel");
}

#[tokio::test]
async fn spin_exhausts_fuel_as_over_budget_and_the_host_survives() {
    let engine = engine();
    let module = compiled(&engine);
    let err = invoke(
        &engine,
        &module,
        "urn:jail:spin",
        allowing(),
        &Limits::fuel(300_000),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "got {err:?}");
    host_survives(&engine, &module).await;
}

#[tokio::test]
async fn balloon_exhausts_memory_as_over_budget_and_the_host_survives() {
    let engine = engine();
    let module = compiled(&engine);
    let limits = Limits::fuel(u64::MAX).with_memory(16 << 20);
    let err = invoke(&engine, &module, "urn:jail:balloon", allowing(), &limits)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "got {err:?}");
    host_survives(&engine, &module).await;
}

#[tokio::test]
async fn sleep_runs_past_a_host_deadline_as_timeout_and_the_host_survives() {
    let engine = engine();
    let module = compiled(&engine);
    let limits = Limits::fuel(u64::MAX).with_deadline(Deadline::host(Duration::from_millis(30)));
    let err = invoke(&engine, &module, "urn:jail:sleep", allowing(), &limits)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Timeout(_)), "got {err:?}");
    host_survives(&engine, &module).await;
}

#[tokio::test]
async fn boom_traps_as_endpoint_and_the_host_survives() {
    let engine = engine();
    let module = compiled(&engine);
    let err = invoke(
        &engine,
        &module,
        "urn:jail:boom",
        allowing(),
        &Limits::fuel(50_000_000),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, Error::Endpoint(_)), "got {err:?}");
    host_survives(&engine, &module).await;
}

// ---------------------------------------------------------------------------
// Mounted into a kernel through ikigai-module's `ModuleSpace` (one module, servable both ways).
// ---------------------------------------------------------------------------

struct PlainRenderer;

impl MetaRenderer for PlainRenderer {
    fn render(&self, d: &Description, _t: &ReprType) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("text/plain"),
            format!("{}: {}", d.id, d.summary).into_bytes(),
        ))
    }
}

/// A kernel whose host space serves `urn:host:name` (requiring [`READ_NAME`]) and whose
/// `urn:jail:` prefix is the jailed guest, mounted through `ModuleSpace::connect`.
async fn kernel_with_the_jail(limits: Limits) -> Kernel {
    let engine = Arc::new(engine());
    let module = Arc::new(compiled(&engine));
    let transport = Arc::new(JailTransport::new(engine, module, limits));
    let jail = ModuleSpace::connect(["urn:jail:"], transport, ModuleFloor::public())
        .await
        .expect("mount the jail");
    let host = EndpointSpace::new().bind(
        Exact::new("urn:host:name"),
        FnEndpoint::new("host-name", |inv: &Invocation<'_>| {
            if !inv.capability.allows(READ_NAME) {
                return Err(Error::Denied("urn:host:name".into()));
            }
            Ok(Representation::new(
                ReprType::new("text/plain"),
                b"camel".to_vec(),
            ))
        }),
    );
    let root: Arc<dyn Space> = Arc::new(Fallback::new(vec![
        Arc::new(host) as Arc<dyn Space>,
        Arc::new(jail) as Arc<dyn Space>,
    ]));
    Kernel::with_meta_renderer(root, Arc::new(PlainRenderer))
}

#[tokio::test]
async fn a_mounted_jail_resolves_through_the_kernel() {
    let kernel = kernel_with_the_jail(Limits::fuel(50_000_000)).await;
    let answer = kernel
        .issue(source("urn:jail:hello"), &allowing())
        .await
        .unwrap();
    assert_eq!(answer.bytes, b"hello, camel");
    // The same read with no grant is a typed denial, through the mount.
    let err = kernel
        .issue(source("urn:jail:hello"), &empty())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Denied(_)), "got {err:?}");
}

#[tokio::test]
async fn a_mounted_jail_cannot_escalate_past_the_invocation() {
    let kernel = kernel_with_the_jail(Limits::fuel(50_000_000)).await;
    let err = kernel
        .issue(source("urn:jail:escalate"), &empty())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Denied(_)), "got {err:?}");
}

#[tokio::test]
async fn a_mounted_jail_reports_its_budget_refusal_typed() {
    let kernel = kernel_with_the_jail(Limits::fuel(300_000)).await;
    let err = kernel
        .issue(source("urn:jail:spin"), &allowing())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "got {err:?}");
}

#[tokio::test]
async fn the_guests_own_cards_describe_its_endpoints_through_the_mount() {
    let kernel = kernel_with_the_jail(Limits::fuel(50_000_000)).await;
    let meta = kernel
        .issue(
            Request::new(CoreVerb::Meta, Iri::parse("urn:jail:hello").unwrap()),
            &allowing(),
        )
        .await
        .unwrap();
    let text = String::from_utf8(meta.bytes).unwrap();
    assert!(
        text.contains("Greet the host's name"),
        "the guest's card reached the catalog: {text}"
    );
}
