//! Always-on, hermetic tests of the component tier: WAT components wasmtime compiles directly
//! (no wasm toolchain), so the default `cargo test` exercises mounting, projection, the budget
//! mapping and the import refusals even where the fixture job does not run.

use std::sync::Arc;

use ikigai_core::{ArgRef, Fallback, Iri, Kernel, Request, Space, Verb};
use ikigai_runtime::{Artifact, Capability, Error, Limits};
use ikigai_runtime_wasmtime::component::{AppConfig, ComponentEngine, World};

/// `acme:math/math`: `add(a: s32, b: s32) -> s32` and a `spin` that never returns.
const MATH: &str = r#"(component
    (core module $m
        (func (export "add") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add)
        (func (export "spin") (result i64) (loop $l (br $l)) i64.const 0))
    (core instance $i (instantiate $m))
    (func $add (param "a" s32) (param "b" s32) (result s32) (canon lift (core func $i "add")))
    (func $spin (result u64) (canon lift (core func $i "spin")))
    (instance $math (export "add" (func $add)) (export "spin" (func $spin)))
    (export "acme:math/math" (instance $math))
)"#;

fn engine() -> ComponentEngine {
    ComponentEngine::new(Limits::fuel(u64::MAX)).expect("a bounded ceiling")
}

async fn math_kernel(limits: Limits) -> Kernel {
    let space = engine()
        .mount(
            &Artifact::from_bytes(MATH.as_bytes().to_vec()),
            AppConfig::new("math", World::Endpoint).limits(limits),
        )
        .await
        .expect("mount the WAT component");
    let root: Arc<dyn Space> = Arc::new(Fallback::new(vec![Arc::new(space) as Arc<dyn Space>]));
    Kernel::new(root)
}

fn add(a: &str, b: &str) -> Request {
    Request::new(
        Verb::Source,
        Iri::parse("urn:runtime:app:math:math:add").unwrap(),
    )
    .with_arg("a", ArgRef::Inline(a.as_bytes().to_vec()))
    .with_arg("b", ArgRef::Inline(b.as_bytes().to_vec()))
}

#[tokio::test]
async fn a_wat_export_projects_and_answers() {
    let kernel = math_kernel(Limits::fuel(1_000_000)).await;
    let sum = kernel
        .issue(add("2", "40"), &Capability::root())
        .await
        .unwrap();
    assert_eq!(sum.bytes, b"42");
}

#[tokio::test]
async fn a_mount_answers_only_under_its_own_prefix_so_sealing_holds() {
    let space = engine()
        .mount(
            &Artifact::from_bytes(MATH.as_bytes().to_vec()),
            AppConfig::new("math", World::Endpoint),
        )
        .await
        .unwrap();
    assert!(space
        .urns()
        .iter()
        .all(|u| u.starts_with("urn:runtime:app:math:")));
    let root = Fallback::new(vec![Arc::new(space) as Arc<dyn Space>]);
    Kernel::check_sealing(&root, ["urn:runtime:artifact:", "urn:runtime:bundle:"])
        .expect("a mount never answers for the runtime's sealed sets");
}

#[tokio::test]
async fn a_missing_argument_is_refused_by_name() {
    let kernel = math_kernel(Limits::fuel(1_000_000)).await;
    let request = Request::new(
        Verb::Source,
        Iri::parse("urn:runtime:app:math:math:add").unwrap(),
    )
    .with_arg("a", ArgRef::Inline(b"1".to_vec()));
    let err = kernel
        .issue(request, &Capability::root())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::MissingArgument(name) if name == "b"),
        "got {err:?}"
    );
}

#[tokio::test]
async fn a_spinning_export_is_over_budget_and_the_next_call_answers() {
    let kernel = math_kernel(Limits::fuel(1_000_000)).await;
    let spin = Request::new(
        Verb::Source,
        Iri::parse("urn:runtime:app:math:math:spin").unwrap(),
    );
    let err = kernel.issue(spin, &Capability::root()).await.unwrap_err();
    assert!(matches!(err, Error::OverBudget { .. }), "got {err:?}");
    let sum = kernel
        .issue(add("1", "2"), &Capability::root())
        .await
        .unwrap();
    assert_eq!(sum.bytes, b"3");
}

#[tokio::test]
async fn an_unbound_import_refuses_the_mount_by_name() {
    let wat = r#"(component
        (import "acme:geo/lookup@1.0.0" (instance
            (export "find" (func (param "place" string) (result string)))))
    )"#;
    let err = engine()
        .mount(
            &Artifact::from_bytes(wat.as_bytes().to_vec()),
            AppConfig::new("geo", World::Endpoint),
        )
        .await
        .err()
        .expect("refused");
    match err {
        Error::Unresolved(iri) => assert_eq!(iri.as_str(), "acme:geo/lookup@1.0.0"),
        other => panic!("expected Unresolved, got {other:?}"),
    }
}

#[tokio::test]
async fn a_wasi_interface_the_runtime_does_not_link_refuses_the_mount() {
    let wat = r#"(component
        (import "wasi:keyvalue/store@0.2.0-draft" (instance
            (export "ping" (func))))
    )"#;
    let err = engine()
        .mount(
            &Artifact::from_bytes(wat.as_bytes().to_vec()),
            AppConfig::new("kv", World::App),
        )
        .await
        .err()
        .expect("refused");
    assert!(matches!(err, Error::Unresolved(_)), "got {err:?}");
}

#[tokio::test]
async fn an_app_name_must_be_one_segment() {
    let err = engine()
        .mount(
            &Artifact::from_bytes(MATH.as_bytes().to_vec()),
            AppConfig::new("a:b", World::Endpoint),
        )
        .await
        .err()
        .expect("refused");
    assert!(matches!(err, Error::InvalidArgument { .. }), "got {err:?}");
}
