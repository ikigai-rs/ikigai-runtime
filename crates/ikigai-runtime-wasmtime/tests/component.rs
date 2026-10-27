//! The component tier, end to end (R-D2, R-D4, R-D5, R-D7, R-D10): an `endpoint`-world component
//! written with the guest SDK (`comp-endpoint`), and a stock component projected export by export
//! with its import bound by configuration (`comp-calc`), each mounted into a real kernel. Behind
//! the `jail-fixtures` feature.
#![cfg(feature = "jail-fixtures")]

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use ikigai_core::{
    ArgRef, Description, EndpointSpace, Exact, Fallback, FnEndpoint, Invocation, Iri, Kernel,
    MetaRenderer, ReprType, Representation, Request, Space, Verb,
};
use ikigai_runtime::{Artifact, Capability, Error, Limits};
use ikigai_runtime_wasmtime::component::{
    AppConfig, ComponentEngine, ComponentSpace, ImportBindings, LoadReport, World,
};
use ikigai_runtime_wasmtime::Metering;

const ENDPOINT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/comp_endpoint.wasm"));
const CALC: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/comp_calc.wasm"));

const READ_NAME: &str = "urn:cap:host:read:name";

struct JsonRenderer;

impl MetaRenderer for JsonRenderer {
    fn render(&self, d: &Description, _t: &ReprType) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/json"),
            serde_json::to_vec(d).unwrap(),
        ))
    }
}

/// The host's own resources: `urn:host:name` (cacheable, counting its reads, gated by
/// [`READ_NAME`]), `urn:host:note` (a Sink and a Source over one cell) and `urn:geo:find` (what
/// `comp-calc`'s `lookup.find` import is bound to).
fn host_space(reads: Arc<AtomicU32>) -> EndpointSpace {
    let note = Arc::new(Mutex::new(Vec::<u8>::new()));
    EndpointSpace::new()
        .bind(
            Exact::new("urn:host:name"),
            FnEndpoint::new("host-name", move |inv: &Invocation<'_>| {
                if !inv.capability.allows(READ_NAME) {
                    return Err(Error::Denied("urn:host:name".into()));
                }
                reads.fetch_add(1, Ordering::SeqCst);
                Ok(
                    Representation::new(ReprType::new("text/plain"), b"camel".to_vec())
                        .cacheable()
                        .depends_on("urn:host:name"),
                )
            }),
        )
        .bind(
            Exact::new("urn:host:note"),
            FnEndpoint::new("host-note", move |inv: &Invocation<'_>| {
                let mut cell = note.lock().unwrap();
                if inv.request.verb == Verb::Sink {
                    *cell = inv.inline_arg("content")?.to_vec();
                }
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    cell.clone(),
                ))
            }),
        )
        .bind(
            Exact::new("urn:geo:find"),
            FnEndpoint::new("geo-find", |inv: &Invocation<'_>| {
                let place = inv.inline_str("place")?;
                let at = if place == "Amsterdam" {
                    "52.37,4.90"
                } else {
                    "?"
                };
                Ok(Representation::new(
                    ReprType::new("text/plain"),
                    at.as_bytes().to_vec(),
                ))
            }),
        )
}

fn kernel(space: ComponentSpace, reads: Arc<AtomicU32>) -> Kernel {
    let root: Arc<dyn Space> = Arc::new(Fallback::new(vec![
        Arc::new(host_space(reads)) as Arc<dyn Space>,
        Arc::new(space) as Arc<dyn Space>,
    ]));
    Kernel::with_meta_renderer(root, Arc::new(JsonRenderer))
}

fn engine() -> ComponentEngine {
    ComponentEngine::new(Limits::fuel(u64::MAX)).expect("a bounded ceiling")
}

fn allowing() -> Capability {
    Capability::root().attenuate([READ_NAME.to_string()])
}

fn empty() -> Capability {
    Capability::root().attenuate(std::iter::empty::<String>())
}

fn source(target: &str) -> Request {
    Request::new(Verb::Source, Iri::parse(target).unwrap())
}

fn with(request: Request, name: &str, value: &str) -> Request {
    request.with_arg(name, ArgRef::Inline(value.as_bytes().to_vec()))
}

fn text(repr: &Representation) -> String {
    String::from_utf8(repr.bytes.clone()).unwrap()
}

// ---------------------------------------------------------------------------
// The `endpoint` world, through the guest SDK's `endpoint-api`.
// ---------------------------------------------------------------------------

async fn endpoint_app(ceiling: Capability, limits: Limits) -> (Kernel, Arc<AtomicU32>) {
    let reads = Arc::new(AtomicU32::new(0));
    let space = engine()
        .mount(
            &Artifact::from_bytes(ENDPOINT.to_vec()),
            AppConfig::new("hello", World::Endpoint)
                .ceiling(ceiling)
                .limits(limits),
        )
        .await
        .expect("mount the endpoint component");
    (kernel(space, Arc::clone(&reads)), reads)
}

#[tokio::test]
async fn describe_names_the_endpoints_the_component_serves() {
    let space = engine()
        .mount(
            &Artifact::from_bytes(ENDPOINT.to_vec()),
            AppConfig::new("hello", World::Endpoint),
        )
        .await
        .unwrap();
    let served = &space.report().served;
    for id in ["hello", "echo", "note", "spin", "balloon", "boom", "clock"] {
        let urn = format!("urn:runtime:app:hello:{id}");
        assert!(served.contains(&urn), "{urn} in {served:?}");
    }
    // Sealed: everything it answers is under its own prefix.
    assert!(space
        .urns()
        .iter()
        .all(|u| u.starts_with("urn:runtime:app:hello:")));
}

#[tokio::test]
async fn the_kernel_import_reads_through_the_invocation_under_the_callers_capability() {
    let (kernel, _) = endpoint_app(Capability::root(), Limits::fuel(100_000_000)).await;
    let answer = kernel
        .issue(source("urn:runtime:app:hello:hello"), &allowing())
        .await
        .unwrap();
    assert_eq!(text(&answer), "hello, camel");
    // Without the grant the kernel import is refused, and the refusal keeps its type across the
    // WIT `error` variant.
    let err = kernel
        .issue(source("urn:runtime:app:hello:hello"), &empty())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Denied(_)), "got {err:?}");
}

#[tokio::test]
async fn the_mount_ceiling_clamps_a_root_caller() {
    // The app's ceiling grants nothing: even a root caller's kernel import is refused.
    let (kernel, _) = endpoint_app(empty(), Limits::fuel(100_000_000)).await;
    let err = kernel
        .issue(source("urn:runtime:app:hello:hello"), &Capability::root())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Denied(_)), "got {err:?}");
}

#[tokio::test]
async fn an_endpoint_world_answer_is_cached_on_its_dependencies() {
    let (kernel, reads) = endpoint_app(Capability::root(), Limits::fuel(100_000_000)).await;
    for _ in 0..3 {
        let answer = kernel
            .issue(source("urn:runtime:app:hello:hello"), &allowing())
            .await
            .unwrap();
        assert_eq!(text(&answer), "hello, camel");
    }
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "the component ran once; the next reads were cache hits"
    );
}

#[tokio::test]
async fn arguments_and_the_sink_verb_cross_the_kernel_import() {
    let (kernel, _) = endpoint_app(Capability::root(), Limits::fuel(100_000_000)).await;
    let echo = kernel
        .issue(
            with(source("urn:runtime:app:hello:echo"), "content", "payload"),
            &allowing(),
        )
        .await
        .unwrap();
    assert_eq!(echo.bytes, b"payload");
    assert_eq!(echo.repr_type.media_type, "application/octet-stream");
    let note = kernel
        .issue(
            with(source("urn:runtime:app:hello:note"), "content", "remember"),
            &allowing(),
        )
        .await
        .unwrap();
    assert_eq!(text(&note), "remember");
}

#[tokio::test]
async fn the_endpoint_world_clock_is_fixed() {
    let (kernel, _) = endpoint_app(Capability::root(), Limits::fuel(100_000_000)).await;
    let answer = kernel
        .issue(source("urn:runtime:app:hello:clock"), &Capability::root())
        .await
        .unwrap();
    assert_eq!(text(&answer), "0", "the Unix epoch, on every machine");
}

#[tokio::test]
async fn budgets_refuse_typed_and_the_host_survives() {
    let (kernel, _) = endpoint_app(
        Capability::root(),
        Limits::fuel(20_000_000).with_memory(16 << 20),
    )
    .await;
    let spin = kernel
        .issue(source("urn:runtime:app:hello:spin"), &allowing())
        .await
        .unwrap_err();
    assert!(matches!(spin, Error::OverBudget { .. }), "got {spin:?}");
    let balloon = kernel
        .issue(source("urn:runtime:app:hello:balloon"), &allowing())
        .await
        .unwrap_err();
    assert!(
        matches!(balloon, Error::OverBudget { .. }),
        "got {balloon:?}"
    );
    let boom = kernel
        .issue(source("urn:runtime:app:hello:boom"), &allowing())
        .await
        .unwrap_err();
    assert!(matches!(boom, Error::Endpoint(_)), "got {boom:?}");
    let ok = kernel
        .issue(source("urn:runtime:app:hello:hello"), &allowing())
        .await
        .unwrap();
    assert_eq!(text(&ok), "hello, camel");
}

#[tokio::test]
async fn a_callers_time_grant_is_a_budget_deadline() {
    // `urn:cap:compute:time:<ms>` is the caller's budget: running past it is OverBudget, never a
    // transient Timeout (R-D10).
    let (kernel, _) = endpoint_app(Capability::root(), Limits::fuel(u64::MAX)).await;
    let caller = Capability::root()
        .attenuate([READ_NAME.to_string(), "urn:cap:compute:time:30".to_string()]);
    let err = kernel
        .issue(source("urn:runtime:app:hello:spin"), &caller)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "got {err:?}");
}

#[tokio::test]
async fn a_compiled_form_is_cached_by_digest() {
    let engine = engine();
    let artifact = Artifact::from_bytes(ENDPOINT.to_vec());
    for app in ["one", "two"] {
        engine
            .mount(&artifact, AppConfig::new(app, World::Endpoint))
            .await
            .unwrap();
    }
    assert_eq!(engine.cached(), 1, "two mounts, one compile");
}

#[tokio::test]
async fn an_epoch_only_engine_refuses_a_fuel_budget_it_cannot_enforce() {
    let engine = ComponentEngine::with_metering(
        Limits::deadline(ikigai_runtime::Deadline::host(
            std::time::Duration::from_secs(5),
        )),
        Metering::EpochOnly,
    )
    .unwrap();
    let space = engine
        .mount(
            &Artifact::from_bytes(ENDPOINT.to_vec()),
            AppConfig::new("hello", World::Endpoint).limits(Limits::fuel(1_000)),
        )
        .await;
    // The mount itself runs describe() under the app's limits, so the refusal comes here.
    assert!(
        matches!(space, Err(Error::InvalidArgument { .. })),
        "a fuel budget on an epoch-only engine is refused, never ignored"
    );
}

// ---------------------------------------------------------------------------
// A stock component, projected (R-D4) with its import bound by configuration (R-D5).
// ---------------------------------------------------------------------------

fn calc_bindings() -> ImportBindings {
    ImportBindings::new()
        .bind("acme:calc/lookup.find", Verb::Source, "urn:geo:find")
        .unwrap()
}

async fn calc_app() -> (Kernel, LoadReport) {
    let space = engine()
        .mount(
            &Artifact::from_bytes(CALC.to_vec()),
            AppConfig::new("calc", World::Endpoint)
                .ceiling(Capability::root())
                .bindings(calc_bindings())
                .limits(Limits::fuel(50_000_000))
                .sink("urn:runtime:app:calc:calc:save"),
        )
        .await
        .expect("mount the calc component");
    let report = space.report().clone();
    (kernel(space, Arc::new(AtomicU32::new(0))), report)
}

async fn calc(kernel: &Kernel, request: Request) -> Result<Representation, Error> {
    kernel.issue(request, &Capability::root()).await
}

#[tokio::test]
async fn every_function_export_projects_and_handles_are_reported_skipped() {
    let (_, report) = calc_app().await;
    for func in [
        "add",
        "greet",
        "shout",
        "summarize",
        "parse",
        "half",
        "paint",
        "locate",
        "save",
        "spin",
    ] {
        let urn = format!("urn:runtime:app:calc:calc:{func}");
        assert!(
            report.projected.contains(&urn),
            "{urn} in {:?}",
            report.projected
        );
    }
    // A function at the root of the world has no interface segment.
    assert!(report
        .projected
        .contains(&"urn:runtime:app:calc:version".to_string()));
    // `open` returns a handle and the counter's methods take one: skipped, with the reason.
    let skipped: Vec<&str> = report.skipped.iter().map(|s| s.export.as_str()).collect();
    assert!(
        skipped.iter().any(|s| s.ends_with("#open")),
        "skipped {skipped:?}"
    );
    assert!(report.skipped.iter().all(|s| s.reason.contains("handle")));
}

#[tokio::test]
async fn scalars_strings_and_bytes_map_to_their_representations() {
    let (kernel, _) = calc_app().await;
    let add = calc(
        &kernel,
        with(
            with(source("urn:runtime:app:calc:calc:add"), "a", "2"),
            "b",
            "40",
        ),
    )
    .await
    .unwrap();
    assert_eq!(text(&add), "42");
    assert_eq!(add.repr_type.media_type, "text/plain");
    let greet = calc(
        &kernel,
        with(source("urn:runtime:app:calc:calc:greet"), "name", "world"),
    )
    .await
    .unwrap();
    assert_eq!(text(&greet), "hello, world");
    let shout = calc(
        &kernel,
        with(source("urn:runtime:app:calc:calc:shout"), "body", "quiet"),
    )
    .await
    .unwrap();
    assert_eq!(shout.bytes, b"QUIET");
    assert_eq!(shout.repr_type.media_type, "application/octet-stream");
    let version = calc(&kernel, source("urn:runtime:app:calc:version"))
        .await
        .unwrap();
    assert_eq!(text(&version), "1.0.0");
}

#[tokio::test]
async fn structures_cross_as_json() {
    let (kernel, _) = calc_app().await;
    let stats = calc(
        &kernel,
        with(
            source("urn:runtime:app:calc:calc:summarize"),
            "xs",
            "[1, 2, 6]",
        ),
    )
    .await
    .unwrap();
    assert_eq!(stats.repr_type.media_type, "application/json");
    let json: serde_json::Value = serde_json::from_slice(&stats.bytes).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"min": 1.0, "max": 6.0, "mean": 3.0})
    );
}

#[tokio::test]
async fn options_results_and_enums_follow_the_type_map() {
    let (kernel, _) = calc_app().await;
    // option: none is NotFound, some(v) is v.
    let odd = calc(
        &kernel,
        with(source("urn:runtime:app:calc:calc:half"), "n", "3"),
    )
    .await
    .unwrap_err();
    assert!(matches!(odd, Error::NotFound(_)), "got {odd:?}");
    let even = calc(
        &kernel,
        with(source("urn:runtime:app:calc:calc:half"), "n", "4"),
    )
    .await
    .unwrap();
    assert_eq!(text(&even), "2");
    // result: ok(v) is v; err(e) is the problem as a VALUE, not a refusal.
    let ok = calc(
        &kernel,
        with(source("urn:runtime:app:calc:calc:parse"), "text", "12"),
    )
    .await
    .unwrap();
    assert_eq!(text(&ok), "12");
    let err = calc(
        &kernel,
        with(source("urn:runtime:app:calc:calc:parse"), "text", "twelve"),
    )
    .await
    .unwrap();
    assert_eq!(err.repr_type.media_type, "application/problem+json");
    assert!(text(&err).contains("is not a number"), "{}", text(&err));
    // enum: a case name; anything else is InvalidArgument. An absent option is none.
    let green = calc(
        &kernel,
        with(source("urn:runtime:app:calc:calc:paint"), "c", "green"),
    )
    .await
    .unwrap();
    assert_eq!(text(&green), "green");
    let shaded = calc(
        &kernel,
        with(
            with(source("urn:runtime:app:calc:calc:paint"), "c", "green"),
            "shade",
            "5",
        ),
    )
    .await
    .unwrap();
    assert_eq!(text(&shaded), "green/5");
    let purple = calc(
        &kernel,
        with(source("urn:runtime:app:calc:calc:paint"), "c", "purple"),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(purple, Error::InvalidArgument { .. }),
        "got {purple:?}"
    );
    // An integer out of range is InvalidArgument, never wrapped.
    let big = calc(
        &kernel,
        with(
            with(source("urn:runtime:app:calc:calc:add"), "a", "3000000000"),
            "b",
            "1",
        ),
    )
    .await
    .unwrap_err();
    assert!(matches!(big, Error::InvalidArgument { .. }), "got {big:?}");
}

#[tokio::test]
async fn a_bound_import_is_issued_through_the_kernel() {
    let (kernel, _) = calc_app().await;
    let located = calc(
        &kernel,
        with(
            source("urn:runtime:app:calc:calc:locate"),
            "place",
            "Amsterdam",
        ),
    )
    .await
    .unwrap();
    assert_eq!(text(&located), "Amsterdam is at 52.37,4.90");
}

#[tokio::test]
async fn an_unbound_import_refuses_the_mount_by_name() {
    let refused = engine()
        .mount(
            &Artifact::from_bytes(CALC.to_vec()),
            AppConfig::new("calc", World::Endpoint),
        )
        .await;
    match refused {
        Err(Error::Unresolved(iri)) => {
            assert!(iri.as_str().contains("acme:calc/lookup"), "named {iri}")
        }
        Err(other) => panic!("expected Unresolved, got {other:?}"),
        Ok(_) => panic!("an unbound import must refuse the mount"),
    }
}

#[tokio::test]
async fn a_projected_endpoint_describes_itself_with_argspecs() {
    let (kernel, _) = calc_app().await;
    let meta = kernel
        .issue(
            Request::new(
                Verb::Meta,
                Iri::parse("urn:runtime:app:calc:calc:paint").unwrap(),
            ),
            &Capability::root(),
        )
        .await
        .unwrap();
    let card: serde_json::Value = serde_json::from_slice(&meta.bytes).unwrap();
    let inputs = card["inputs"].as_array().unwrap();
    let c = inputs.iter().find(|i| i["name"] == "c").unwrap();
    assert_eq!(c["one_of"], serde_json::json!(["red", "green", "blue"]));
    let shade = inputs.iter().find(|i| i["name"] == "shade").unwrap();
    assert_eq!(shade["required"], serde_json::json!(false));
    assert_eq!(
        shade["class"],
        "http://www.w3.org/2001/XMLSchema#unsignedByte"
    );
    // The summary is the WIT doc comment the binary carries (`package-docs`), when the guest's
    // toolchain kept it; the fallback names the export.
    let summary = card["summary"].as_str().unwrap();
    assert!(
        summary == "Name a color, with an optional shade."
            || summary.contains("acme:calc/calc@1.0.0#paint"),
        "summary {summary}"
    );
}

#[tokio::test]
async fn a_declared_sink_takes_its_content() {
    let (kernel, _) = calc_app().await;
    let saved = kernel
        .issue(
            Request::new(
                Verb::Sink,
                Iri::parse("urn:runtime:app:calc:calc:save").unwrap(),
            )
            .with_arg("content", ArgRef::Inline(b"hello".to_vec())),
            &Capability::root(),
        )
        .await
        .unwrap();
    assert_eq!(text(&saved), "5");
}

#[tokio::test]
async fn two_exports_projecting_to_one_name_refuse_the_mount() {
    let wat = r#"(component
        (core module $m (func (export "f") (result i32) i32.const 1))
        (core instance $i (instantiate $m))
        (func $f (result u32) (canon lift (core func $i "f")))
        (instance $a (export "f" (func $f)))
        (instance $b (export "f" (func $f)))
        (export "acme:one/calc" (instance $a))
        (export "acme:two/calc" (instance $b))
    )"#;
    let refused = engine()
        .mount(
            &Artifact::from_bytes(wat.as_bytes().to_vec()),
            AppConfig::new("dup", World::Endpoint),
        )
        .await;
    match refused {
        Err(Error::Conflict(message)) => {
            assert!(
                message.contains("acme:one/calc") && message.contains("acme:two/calc"),
                "names both: {message}"
            )
        }
        Err(other) => panic!("expected Conflict, got {other:?}"),
        Ok(_) => panic!("a load-order-dependent name must refuse the mount"),
    }
}

#[tokio::test]
async fn a_stock_export_spends_its_fuel_budget_as_over_budget() {
    let (kernel, _) = calc_app().await;
    let err = calc(&kernel, source("urn:runtime:app:calc:calc:spin"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "got {err:?}");
}
