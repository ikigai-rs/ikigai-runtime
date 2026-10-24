//! Always-on, hermetic tests of the engine: the numeric convention, the budget → error mapping
//! (R-D10), and containment (the host survives a trapped run). These use WAT modules that
//! wasmtime compiles directly (no wasm toolchain), so `cargo test` with no features exercises the
//! engine. The full ABI v1 session and the WASI p1 path are covered by the guest fixtures behind
//! the `jail-fixtures` feature.

use std::sync::Arc;
use std::time::Duration;

use ikigai_runtime::space::Convention;
use ikigai_runtime::{
    Call, Capability, CoreEngine, Deadline, Error, HostResolver, Limits, Output, Value,
};
use ikigai_runtime_wasmtime::{detect_convention, WasmtimeCoreEngine};

struct NoHost;
impl HostResolver for NoHost {
    fn source(&self, _iri: &str, _cap: &Capability) -> Result<Vec<u8>, Error> {
        Err(Error::NotFound("this test has no host".into()))
    }
}

const ADD: &str = r#"(module (func (export "add") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add))"#;
const SPIN: &str = r#"(module (func (export "spin") (loop $l (br $l))))"#;

fn engine() -> WasmtimeCoreEngine {
    // A generous host ceiling (bounded, as the runtime requires) that the per-call caller limits
    // then tighten. Large so it does not clip the epoch tests, which bound by deadline.
    WasmtimeCoreEngine::new(Limits::fuel(u64::MAX)).expect("a bounded ceiling")
}

#[test]
fn an_unbounded_ceiling_is_refused_so_the_runtime_will_not_start() {
    assert!(matches!(
        WasmtimeCoreEngine::new(Limits::unbounded()),
        Err(ikigai_runtime::EngineError::Unbounded)
    ));
}

#[test]
fn the_numeric_convention_calls_a_numeric_export() {
    let engine = engine();
    let compiled = engine.compile(ADD.as_bytes()).unwrap();
    assert_eq!(detect_convention(&compiled), Convention::Numeric);
    let out = engine
        .invoke(
            &compiled,
            Call::Numeric {
                export: "add",
                args: &[Value::I32(2), Value::I32(40)],
            },
            &Limits::fuel(1_000_000),
            Arc::new(NoHost),
        )
        .unwrap();
    match out.output {
        Output::Numeric(vals) => assert_eq!(vals, vec![Value::I32(42)]),
        _ => panic!("expected a numeric output"),
    }
    assert!(out.consumed.fuel.is_some(), "fuel consumption is recorded");
}

#[test]
fn fuel_exhaustion_is_over_budget_never_transient() {
    let engine = engine();
    let compiled = engine.compile(SPIN.as_bytes()).unwrap();
    let err = engine
        .invoke(
            &compiled,
            Call::Numeric {
                export: "spin",
                args: &[],
            },
            &Limits::fuel(100_000),
            Arc::new(NoHost),
        )
        .unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "got {err:?}");
}

#[test]
fn a_host_imposed_deadline_is_a_timeout() {
    let engine = engine();
    let compiled = engine.compile(SPIN.as_bytes()).unwrap();
    // Huge fuel, so only the epoch deadline ends the spin; a HOST deadline → Timeout.
    let limits = Limits::fuel(u64::MAX).with_deadline(Deadline::host(Duration::from_millis(30)));
    let err = engine
        .invoke(
            &compiled,
            Call::Numeric {
                export: "spin",
                args: &[],
            },
            &limits,
            Arc::new(NoHost),
        )
        .unwrap_err();
    assert!(matches!(err, Error::Timeout(_)), "got {err:?}");
}

#[test]
fn a_caller_budget_deadline_is_over_budget() {
    let engine = engine();
    let compiled = engine.compile(SPIN.as_bytes()).unwrap();
    // A BUDGET deadline → OverBudget, never transient.
    let limits = Limits::fuel(u64::MAX).with_deadline(Deadline::budget(Duration::from_millis(30)));
    let err = engine
        .invoke(
            &compiled,
            Call::Numeric {
                export: "spin",
                args: &[],
            },
            &limits,
            Arc::new(NoHost),
        )
        .unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "got {err:?}");
}

#[test]
fn the_host_survives_a_trapped_run_and_answers_the_next() {
    let engine = engine();
    let spin = engine.compile(SPIN.as_bytes()).unwrap();
    let add = engine.compile(ADD.as_bytes()).unwrap();
    // Trap on fuel...
    assert!(engine
        .invoke(
            &spin,
            Call::Numeric {
                export: "spin",
                args: &[]
            },
            &Limits::fuel(50_000),
            Arc::new(NoHost)
        )
        .is_err());
    // ...then the next request on the same engine resolves normally (containment).
    let out = engine
        .invoke(
            &add,
            Call::Numeric {
                export: "add",
                args: &[Value::I32(1), Value::I32(2)],
            },
            &Limits::fuel(1_000_000),
            Arc::new(NoHost),
        )
        .unwrap();
    match out.output {
        Output::Numeric(vals) => assert_eq!(vals, vec![Value::I32(3)]),
        _ => panic!("expected a numeric output"),
    }
}
