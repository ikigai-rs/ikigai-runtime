//! The ikigai-module ABI v1 (J2) session tier, end to end through the `jail-guest` fixture
//! (R-D6, R-D18). Behind the `jail-fixtures` feature, which the build script and the dedicated
//! CI job turn on with the wasm targets installed.
#![cfg(feature = "jail-fixtures")]

use std::sync::Arc;
use std::time::Duration;

use ikigai_runtime::session::{ModuleCall, ModuleReply};
use ikigai_runtime::space::Convention;
use ikigai_runtime::{Call, Capability, CoreEngine, Deadline, Error, HostResolver, Limits, Output};
use ikigai_runtime_wasmtime::{detect_convention, Compiled, WasmtimeCoreEngine};

/// The guest wasm, built by `build.rs` under the `jail-fixtures` feature.
const JAIL_GUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/jail_guest.wasm"));

/// A host that serves `urn:host:name`, but only to a capability that allows reading it — so the
/// capability floor is observable across the jail.
struct HostName;

impl HostResolver for HostName {
    fn source(&self, iri: &str, capability: &Capability) -> Result<Vec<u8>, Error> {
        if iri != "urn:host:name" {
            return Err(Error::NotFound(iri.to_string()));
        }
        if !capability.allows("urn:cap:host:read:name") {
            return Err(Error::Denied("urn:host:name".to_string()));
        }
        Ok(b"camel".to_vec())
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
    Capability::root().attenuate(["urn:cap:host:read:name".to_string()])
}

/// A capability that allows nothing.
fn empty() -> Capability {
    Capability::root().attenuate(std::iter::empty::<String>())
}

/// Encode an `Invoke`, run it through the session convention, and decode the `ModuleReply`.
fn invoke(
    engine: &WasmtimeCoreEngine,
    module: &Compiled,
    endpoint: &str,
    capability: Capability,
    limits: &Limits,
    host: Arc<dyn HostResolver>,
) -> Result<(ModuleReply, Option<u64>), Error> {
    let call = ModuleCall::Invoke {
        endpoint: endpoint.to_string(),
        body: Vec::new(),
        capability,
    };
    let encoded = postcard::to_allocvec(&call).unwrap();
    let invoked = engine.invoke(
        module,
        Call::Session {
            encoded_call: &encoded,
        },
        limits,
        host,
    )?;
    let bytes = match invoked.output {
        Output::Session(bytes) => bytes,
        _ => panic!("expected a session output"),
    };
    Ok((postcard::from_bytes(&bytes).unwrap(), invoked.consumed.fuel))
}

#[test]
fn the_guest_detects_as_the_session_convention() {
    let engine = engine();
    assert_eq!(detect_convention(&compiled(&engine)), Convention::Session);
}

#[test]
fn hello_sources_the_host_name_through_a_host_call() {
    let engine = engine();
    let module = compiled(&engine);
    let (reply, _) = invoke(
        &engine,
        &module,
        "urn:jail:hello",
        allowing(),
        &Limits::fuel(50_000_000),
        Arc::new(HostName),
    )
    .unwrap();
    assert_eq!(reply, ModuleReply::Resolved(b"hello, camel".to_vec()));
}

#[test]
fn the_capability_floor_holds_across_the_jail() {
    let engine = engine();
    let module = compiled(&engine);
    // `escalate` claims root on its host_call, but the session grants nothing: the host clamps it,
    // so the read is denied — a guest cannot widen its authority by being jailed.
    let (reply, _) = invoke(
        &engine,
        &module,
        "urn:jail:escalate",
        empty(),
        &Limits::fuel(50_000_000),
        Arc::new(HostName),
    )
    .unwrap();
    match reply {
        ModuleReply::Error(message) => assert!(message.contains("urn:host:name"), "got {message}"),
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
        Arc::new(HostName),
    )
    .unwrap();
    assert_eq!(ok, ModuleReply::Resolved(b"camel".to_vec()));
}

#[test]
fn describe_returns_the_contract() {
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
        .unwrap();
    let bytes = match invoked.output {
        Output::Session(b) => b,
        _ => panic!("expected a session output"),
    };
    match postcard::from_bytes::<ModuleReply>(&bytes).unwrap() {
        ModuleReply::Describe(json) => assert!(json.contains("urn:jail:hello"), "got {json}"),
        other => panic!("expected Describe, got {other:?}"),
    }
}

#[test]
fn fuel_consumed_by_hello_is_identical_across_two_runs() {
    let engine = engine();
    let module = compiled(&engine);
    let (_, a) = invoke(
        &engine,
        &module,
        "urn:jail:hello",
        allowing(),
        &Limits::fuel(50_000_000),
        Arc::new(HostName),
    )
    .unwrap();
    let (_, b) = invoke(
        &engine,
        &module,
        "urn:jail:hello",
        allowing(),
        &Limits::fuel(50_000_000),
        Arc::new(HostName),
    )
    .unwrap();
    assert!(a.is_some());
    assert_eq!(a, b, "fuel is a deterministic cost");
}

#[test]
fn spin_exhausts_fuel_as_over_budget_and_the_host_survives() {
    let engine = engine();
    let module = compiled(&engine);
    let err = invoke(
        &engine,
        &module,
        "urn:jail:spin",
        allowing(),
        &Limits::fuel(300_000),
        Arc::new(HostName),
    )
    .unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "got {err:?}");
    // The host answers the next request.
    let (ok, _) = invoke(
        &engine,
        &module,
        "urn:jail:hello",
        allowing(),
        &Limits::fuel(50_000_000),
        Arc::new(HostName),
    )
    .unwrap();
    assert_eq!(ok, ModuleReply::Resolved(b"hello, camel".to_vec()));
}

#[test]
fn balloon_exhausts_memory_as_over_budget_and_the_host_survives() {
    let engine = engine();
    let module = compiled(&engine);
    let limits = Limits::fuel(u64::MAX).with_memory(16 << 20);
    let err = invoke(
        &engine,
        &module,
        "urn:jail:balloon",
        allowing(),
        &limits,
        Arc::new(HostName),
    )
    .unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "got {err:?}");
    let (ok, _) = invoke(
        &engine,
        &module,
        "urn:jail:hello",
        allowing(),
        &Limits::fuel(50_000_000),
        Arc::new(HostName),
    )
    .unwrap();
    assert_eq!(ok, ModuleReply::Resolved(b"hello, camel".to_vec()));
}

#[test]
fn sleep_runs_past_a_host_deadline_as_timeout_and_the_host_survives() {
    let engine = engine();
    let module = compiled(&engine);
    let limits = Limits::fuel(u64::MAX).with_deadline(Deadline::host(Duration::from_millis(30)));
    let err = invoke(
        &engine,
        &module,
        "urn:jail:sleep",
        allowing(),
        &limits,
        Arc::new(HostName),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Timeout(_)), "got {err:?}");
    let (ok, _) = invoke(
        &engine,
        &module,
        "urn:jail:hello",
        allowing(),
        &Limits::fuel(50_000_000),
        Arc::new(HostName),
    )
    .unwrap();
    assert_eq!(ok, ModuleReply::Resolved(b"hello, camel".to_vec()));
}

#[test]
fn boom_traps_as_endpoint_and_the_host_survives() {
    let engine = engine();
    let module = compiled(&engine);
    let err = invoke(
        &engine,
        &module,
        "urn:jail:boom",
        allowing(),
        &Limits::fuel(50_000_000),
        Arc::new(HostName),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Endpoint(_)), "got {err:?}");
    let (ok, _) = invoke(
        &engine,
        &module,
        "urn:jail:hello",
        allowing(),
        &Limits::fuel(50_000_000),
        Arc::new(HostName),
    )
    .unwrap();
    assert_eq!(ok, ModuleReply::Resolved(b"hello, camel".to_vec()));
}
