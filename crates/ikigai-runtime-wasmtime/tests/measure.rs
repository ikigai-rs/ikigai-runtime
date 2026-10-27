//! Measurement harnesses for two "Still to measure (R3 owns)" items (R-D7, R-D10). They are set
//! up here so the measurement is a follow-on: run them with `cargo test -- --ignored --nocapture`.
//! They are `#[ignore]`d so they never gate CI (timings are not assertions).
//!
//! Still pending (not set up here, by scope): waffle's state/coverage/cost and Wizer's component
//! support (R-D10), and `package-docs` retention (R-D4) — all component-tier or build-tooling
//! questions for R3b. The WASI p1 path (R-D6) is NOT pending: `tests/wasi_p1.rs` verifies it.

use std::sync::Arc;
use std::time::Instant;

use ikigai_runtime::{Call, CoreEngine, Limits, NoHost, Value};
use ikigai_runtime_wasmtime::WasmtimeCoreEngine;

const ADD: &str = r#"(module (func (export "add") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add))"#;
const SPIN: &str = r#"(module (func (export "spin") (loop $l (br $l))))"#;

/// The fuel-per-millisecond calibration fixture (R-D10): spend a known fuel budget on a spin loop,
/// time it, and report the rate. R1's 3.4e9 units/s is an upper bound for the cheapest loop; this
/// harness is where a real figure for this host is taken.
#[tokio::test]
#[ignore = "calibration harness: run with --ignored --nocapture"]
async fn calibrate_fuel_per_millisecond() {
    let engine = WasmtimeCoreEngine::new(Limits::fuel(u64::MAX)).unwrap();
    let compiled = engine.compile(SPIN.as_bytes()).unwrap();
    let fuel: u64 = 2_000_000_000;
    let start = Instant::now();
    let _ = engine
        .invoke(
            &compiled,
            Call::Numeric {
                export: "spin",
                args: &[],
            },
            &Limits::fuel(fuel),
            Arc::new(NoHost),
        )
        .await;
    let elapsed = start.elapsed();
    let per_ms = fuel as f64 / elapsed.as_secs_f64() / 1_000.0;
    println!("calibration: {fuel} fuel in {elapsed:?} = {per_ms:.0} units/ms");
}

/// The pooling / instantiation-cost fixture (R-D7): compile once, then time many
/// instantiate-and-call cycles. A baseline for whether `PoolingAllocationConfig` / `InstancePre`
/// reuse is worth adding.
#[tokio::test]
#[ignore = "pooling harness: run with --ignored --nocapture"]
async fn measure_instantiation_cost() {
    let engine = WasmtimeCoreEngine::new(Limits::fuel(u64::MAX)).unwrap();
    let compiled = engine.compile(ADD.as_bytes()).unwrap();
    let n = 10_000u32;
    let start = Instant::now();
    for i in 0..n {
        let _ = engine
            .invoke(
                &compiled,
                Call::Numeric {
                    export: "add",
                    args: &[Value::I32(i as i32), Value::I32(1)],
                },
                &Limits::fuel(1_000_000),
                Arc::new(NoHost),
            )
            .await
            .unwrap();
    }
    let elapsed = start.elapsed();
    println!(
        "instantiation: {n} fresh-store calls in {elapsed:?} = {:?}/call",
        elapsed / n
    );
}
