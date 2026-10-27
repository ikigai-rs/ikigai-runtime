//! Measurement harnesses for the "Still to measure (R3 owns)" items (R-D7, R-D10). Run them with
//! `cargo test -p ikigai-runtime-wasmtime --release --test measure -- --ignored --nocapture`
//! (add `--features jail-fixtures` for the std-guest harness). They are `#[ignore]`d so they never
//! gate CI: timings are not assertions. R3b recorded their numbers in its hand-back.
//!
//! Still pending, by scope: waffle's state, coverage and cost, and Wizer's component support
//! (R-D10). Measured elsewhere: `package-docs` retention (R-D4) is `tests/component.rs`'s
//! summary test; the WASI p1 path (R-D6) is `tests/wasi_p1.rs`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ikigai_core::{ArgRef, Fallback, Iri, Kernel, Request, Space, Verb};
use ikigai_runtime::{Artifact, Call, Capability, CoreEngine, Deadline, Limits, NoHost, Value};
use ikigai_runtime_wasmtime::component::{AppConfig, ComponentEngine, World};
use ikigai_runtime_wasmtime::{Metering, WasmtimeCoreEngine};

const ADD: &str = r#"(module (func (export "add") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add))"#;
const SPIN: &str = r#"(module (func (export "spin") (loop $l (br $l))))"#;
/// Count down from the argument: a compute-bound loop that finishes.
const COUNT: &str = r#"(module (func (export "count") (param i64) (result i64)
    (loop $l
        local.get 0 i64.const 1 i64.sub local.tee 0
        i64.const 0 i64.ne br_if $l)
    local.get 0))"#;

const MATH: &str = r#"(component
    (core module $m
        (func (export "add") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add))
    (core instance $i (instantiate $m))
    (func $add (param "a" s32) (param "b" s32) (result s32) (canon lift (core func $i "add")))
    (instance $math (export "add" (func $add)))
    (export "acme:math/math" (instance $math))
)"#;

/// The fuel-per-millisecond calibration fixture (R-D10): spend a known fuel budget on a spin loop,
/// time it, and report the rate. R1's 3.4e9 units/s is an upper bound for the cheapest loop; this
/// harness is where a real figure for this host is taken.
#[tokio::test]
#[ignore = "calibration harness: run with --ignored --nocapture"]
async fn calibrate_fuel_per_millisecond() {
    let engine = WasmtimeCoreEngine::new(Limits::fuel(u64::MAX)).unwrap();
    let compiled = engine.compile(SPIN.as_bytes()).unwrap();
    for fuel in [200_000_000u64, 2_000_000_000] {
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
}

/// Fuel's cost against an epoch-only engine (R-D10, R1: "up to 2-3x"): the same compute-bound
/// loop on both, best of five.
#[tokio::test]
#[ignore = "metering harness: run with --ignored --nocapture"]
async fn compare_fuel_and_epoch_only_metering() {
    let iterations = 200_000_000i64;
    for metering in [Metering::FuelAndEpoch, Metering::EpochOnly] {
        let ceiling = Limits::deadline(Deadline::host(Duration::from_secs(60)));
        let engine = WasmtimeCoreEngine::with_metering(ceiling, metering).unwrap();
        let compiled = engine.compile(COUNT.as_bytes()).unwrap();
        let mut best = Duration::MAX;
        for _ in 0..5 {
            let start = Instant::now();
            engine
                .invoke(
                    &compiled,
                    Call::Numeric {
                        export: "count",
                        args: &[Value::I64(iterations)],
                    },
                    &Limits::unbounded(),
                    Arc::new(NoHost),
                )
                .await
                .unwrap();
            best = best.min(start.elapsed());
        }
        println!("metering {metering:?}: {iterations} iterations in {best:?} (best of 5)");
    }
}

/// The core-module tier's per-call cost (R-D7): compile once, then time many
/// instantiate-and-call cycles, each in a fresh `Store`.
#[tokio::test]
#[ignore = "instantiation harness: run with --ignored --nocapture"]
async fn measure_core_module_instantiation_cost() {
    let engine = WasmtimeCoreEngine::new(Limits::fuel(u64::MAX)).unwrap();
    let compiled = engine.compile(ADD.as_bytes()).unwrap();
    let n = 10_000u32;
    let start = Instant::now();
    for i in 0..n {
        engine
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
        "core module: {n} fresh-store calls in {elapsed:?} = {:?}/call",
        elapsed / n
    );
}

/// The component tier's per-call cost through the kernel (R-D7): a mounted WAT component, each
/// call with new arguments (so no cache hit), each in a fresh `Store` from the cached
/// `InstancePre`, with the WASI context and kernel seam built per call.
#[tokio::test]
#[ignore = "instantiation harness: run with --ignored --nocapture"]
async fn measure_component_call_cost_through_the_kernel() {
    let space = ComponentEngine::new(Limits::fuel(u64::MAX))
        .unwrap()
        .mount(
            &Artifact::from_bytes(MATH.as_bytes().to_vec()),
            AppConfig::new("math", World::Endpoint).limits(Limits::fuel(1_000_000)),
        )
        .await
        .unwrap();
    let kernel = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(space) as Arc<dyn Space>
    ])));
    let target = Iri::parse("urn:runtime:app:math:math:add").unwrap();
    let n = 5_000u32;
    let start = Instant::now();
    for i in 0..n {
        let request = Request::new(Verb::Source, target.clone())
            .with_arg("a", ArgRef::Inline(i.to_string().into_bytes()))
            .with_arg("b", ArgRef::Inline(b"1".to_vec()));
        kernel.issue(request, &Capability::root()).await.unwrap();
    }
    let elapsed = start.elapsed();
    println!(
        "component (WAT) through the kernel: {n} calls in {elapsed:?} = {:?}/call",
        elapsed / n
    );
}

/// What `InstancePre` reuse and the pooling allocator buy (R-D7), on wasmtime directly: the
/// same component, linked with WASI p2, instantiated and called per call (a) re-linking every
/// time, (b) from one `InstancePre`, (c) from one `InstancePre` on a pooling engine.
#[tokio::test]
#[ignore = "instantiation harness: run with --ignored --nocapture"]
async fn measure_instance_pre_and_pooling() {
    use wasmtime::component::{Component, Linker, ResourceTable};
    use wasmtime::{Config, Engine, InstanceAllocationStrategy, PoolingAllocationConfig, Store};
    use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

    struct St {
        ctx: WasiCtx,
        table: ResourceTable,
    }
    impl WasiView for St {
        fn ctx(&mut self) -> WasiCtxView<'_> {
            WasiCtxView {
                ctx: &mut self.ctx,
                table: &mut self.table,
            }
        }
    }
    let store_of = |engine: &Engine| {
        Store::new(
            engine,
            St {
                ctx: WasiCtx::builder().build(),
                table: ResourceTable::new(),
            },
        )
    };
    let n = 5_000u32;
    for (label, pooling, relink) in [
        ("re-link per call", false, true),
        ("InstancePre", false, false),
        ("InstancePre + pooling", true, false),
    ] {
        let mut config = Config::new();
        if pooling {
            config.allocation_strategy(InstanceAllocationStrategy::Pooling(
                PoolingAllocationConfig::default(),
            ));
        }
        let engine = Engine::new(&config).unwrap();
        let component = Component::new(&engine, MATH).unwrap();
        let link = || {
            let mut linker: Linker<St> = Linker::new(&engine);
            wasmtime_wasi::p2::add_to_linker_async(&mut linker).unwrap();
            linker
        };
        let pre = link().instantiate_pre(&component).unwrap();
        let start = Instant::now();
        for i in 0..n {
            let mut store = store_of(&engine);
            let instance = if relink {
                link()
                    .instantiate_async(&mut store, &component)
                    .await
                    .unwrap()
            } else {
                pre.instantiate_async(&mut store).await.unwrap()
            };
            let iface = instance
                .get_export_index(&mut store, None, "acme:math/math")
                .unwrap();
            let idx = instance
                .get_export_index(&mut store, Some(&iface), "add")
                .unwrap();
            let add = instance
                .get_typed_func::<(i32, i32), (i32,)>(&mut store, idx)
                .unwrap();
            let (sum,) = add.call_async(&mut store, (i as i32, 1)).await.unwrap();
            assert_eq!(sum, i as i32 + 1);
        }
        let elapsed = start.elapsed();
        println!(
            "wasmtime {label}: {n} calls in {elapsed:?} = {:?}/call",
            elapsed / n
        );
    }
}

/// A Rust std component's per-call cost through the kernel (the `comp-calc` fixture's `add`):
/// the realistic case, with the full WASI p2 linker and a context built per call.
#[cfg(feature = "jail-fixtures")]
#[tokio::test]
#[ignore = "instantiation harness: run with --ignored --nocapture --features jail-fixtures"]
async fn measure_std_component_call_cost_through_the_kernel() {
    const CALC: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/comp_calc.wasm"));
    let space = ComponentEngine::new(Limits::fuel(u64::MAX))
        .unwrap()
        .mount(
            &Artifact::from_bytes(CALC.to_vec()),
            AppConfig::new("calc", World::Endpoint)
                .bindings(
                    ikigai_runtime_wasmtime::component::ImportBindings::new()
                        .bind("acme:calc/lookup.find", Verb::Source, "urn:geo:find")
                        .unwrap(),
                )
                .limits(Limits::fuel(50_000_000)),
        )
        .await
        .unwrap();
    let kernel = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(space) as Arc<dyn Space>
    ])));
    let target = Iri::parse("urn:runtime:app:calc:calc:add").unwrap();
    let n = 2_000u32;
    let start = Instant::now();
    for i in 0..n {
        let request = Request::new(Verb::Source, target.clone())
            .with_arg("a", ArgRef::Inline(i.to_string().into_bytes()))
            .with_arg("b", ArgRef::Inline(b"1".to_vec()));
        kernel.issue(request, &Capability::root()).await.unwrap();
    }
    let elapsed = start.elapsed();
    println!(
        "component (Rust std, wasip2) through the kernel: {n} calls in {elapsed:?} = {:?}/call",
        elapsed / n
    );
}
