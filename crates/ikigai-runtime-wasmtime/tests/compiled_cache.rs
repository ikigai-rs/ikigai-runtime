//! The host's on-disk compiled-form cache (R-D7): a restarted host loads what it wrote instead of
//! compiling, for both tiers, and anything it cannot trust is compiled again and reported.

use std::path::PathBuf;
use std::sync::Arc;

use ikigai_runtime::{Artifact, Call, CoreEngine, Limits, NoHost, Value};
use ikigai_runtime_wasmtime::component::ComponentEngine;
use ikigai_runtime_wasmtime::{CompiledCache, Metering, Origin, WasmtimeCoreEngine};

const ADD: &str = r#"(module (func (export "add") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add))"#;
const SUB: &str = r#"(module (func (export "sub") (param i32 i32) (result i32) local.get 0 local.get 1 i32.sub))"#;
const MATH: &str = r#"(component
    (core module $m
        (func (export "add") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add))
    (core instance $i (instantiate $m))
    (func $add (param "a" s32) (param "b" s32) (result s32) (canon lift (core func $i "add")))
    (instance $math (export "add" (func $add)))
    (export "acme:math/math" (instance $math))
)"#;

/// A fresh, empty cache directory for one test.
fn cache_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ikigai-compiled-cache-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A "restart": a new engine, so nothing is in memory, over the same directory.
fn core(dir: &PathBuf) -> WasmtimeCoreEngine {
    WasmtimeCoreEngine::new(Limits::fuel(1_000_000))
        .unwrap()
        .with_compiled_cache(CompiledCache::new(dir))
}

fn component(dir: &PathBuf) -> ComponentEngine {
    ComponentEngine::new(Limits::fuel(1_000_000))
        .unwrap()
        .with_compiled_cache(CompiledCache::new(dir))
}

async fn add(engine: &WasmtimeCoreEngine, compiled: &ikigai_runtime_wasmtime::Compiled) -> Value {
    let invoked = engine
        .invoke(
            compiled,
            Call::Numeric {
                export: "add",
                args: &[Value::I32(2), Value::I32(3)],
            },
            &Limits::fuel(1_000),
            Arc::new(NoHost),
        )
        .await
        .unwrap();
    match invoked.output {
        ikigai_runtime::Output::Numeric(values) => values[0],
        other => panic!("not a numeric answer: {other:?}"),
    }
}

#[tokio::test]
async fn a_restarted_core_engine_loads_the_compiled_form_it_wrote() {
    let dir = cache_dir("core-restart");
    let (compiled, outcome) = core(&dir).compile_traced(ADD.as_bytes()).unwrap();
    assert_eq!(outcome.origin, Origin::Compiled, "{outcome}");
    assert!(outcome.notes.is_empty(), "{outcome}");
    drop(compiled);

    let engine = core(&dir);
    let (compiled, outcome) = engine.compile_traced(ADD.as_bytes()).unwrap();
    assert_eq!(outcome.origin, Origin::Disk, "{outcome}");
    assert!(outcome.notes.is_empty(), "{outcome}");
    assert_eq!(compiled.exports(), ["add"]);
    assert_eq!(add(&engine, &compiled).await, Value::I32(5));
}

#[tokio::test]
async fn without_a_cache_a_restarted_engine_compiles_again() {
    let engine = WasmtimeCoreEngine::new(Limits::fuel(1_000_000)).unwrap();
    for _ in 0..2 {
        let (_, outcome) = engine.compile_traced(ADD.as_bytes()).unwrap();
        assert_eq!(outcome.origin, Origin::Compiled);
    }
}

#[test]
fn a_restarted_component_engine_loads_the_compiled_form_it_wrote() {
    let dir = cache_dir("component-restart");
    let artifact = Artifact::from_bytes(MATH.as_bytes().to_vec());
    let first = component(&dir);
    let (_, outcome) = first.compile_traced(&artifact).unwrap();
    assert_eq!(outcome.origin, Origin::Compiled, "{outcome}");
    let (_, outcome) = first.compile_traced(&artifact).unwrap();
    assert_eq!(outcome.origin, Origin::Memory, "{outcome}");
    drop(first);

    let (_, outcome) = component(&dir).compile_traced(&artifact).unwrap();
    assert_eq!(outcome.origin, Origin::Disk, "{outcome}");
    assert!(outcome.notes.is_empty(), "{outcome}");
}

#[tokio::test]
async fn a_mounted_component_from_the_disk_cache_answers() {
    use ikigai_core::{ArgRef, Fallback, Iri, Kernel, Request, Space, Verb};
    use ikigai_runtime_wasmtime::component::{AppConfig, World};
    let dir = cache_dir("component-mount");
    let artifact = Artifact::from_bytes(MATH.as_bytes().to_vec());
    component(&dir).compile(&artifact).unwrap();

    let engine = component(&dir);
    let space = engine
        .mount(&artifact, AppConfig::new("math", World::Endpoint))
        .await
        .unwrap();
    assert_eq!(
        engine.compile_traced(&artifact).unwrap().1.origin,
        Origin::Memory
    );
    let kernel = Kernel::new(Arc::new(Fallback::new(vec![
        Arc::new(space) as Arc<dyn Space>
    ])));
    let request = Request::new(
        Verb::Source,
        Iri::parse("urn:runtime:app:math:math:add").unwrap(),
    )
    .with_arg("a", ArgRef::Inline(b"2".to_vec()))
    .with_arg("b", ArgRef::Inline(b"3".to_vec()));
    let answer = kernel
        .issue(request, &ikigai_runtime::Capability::root())
        .await
        .unwrap();
    assert_eq!(answer.bytes, b"5");
}

#[tokio::test]
async fn a_corrupt_entry_is_compiled_again_and_says_so() {
    let dir = cache_dir("corrupt");
    let engine = core(&dir);
    let digest = ikigai_runtime::ArtifactDigest::of(ADD.as_bytes());
    let path = CompiledCache::new(&dir).path_of(&ikigai_runtime::CompiledKey::new(
        digest,
        engine.compat_hash(),
    ));
    engine.compile_traced(ADD.as_bytes()).unwrap();

    // One flipped byte in the body: the hash no longer matches, so it never reaches deserialize.
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    std::fs::write(&path, &bytes).unwrap();
    let engine = core(&dir);
    let (compiled, outcome) = engine.compile_traced(ADD.as_bytes()).unwrap();
    assert_eq!(outcome.origin, Origin::Compiled);
    assert_eq!(outcome.notes.len(), 1, "{outcome}");
    assert!(outcome.notes[0].contains("is corrupt"), "{outcome}");
    assert!(
        outcome.notes[0].contains(&path.display().to_string()),
        "the note names the file: {outcome}"
    );
    assert_eq!(add(&engine, &compiled).await, Value::I32(5));

    // The entry was rewritten, so the next restart loads it.
    let (_, outcome) = core(&dir).compile_traced(ADD.as_bytes()).unwrap();
    assert_eq!(outcome.origin, Origin::Disk, "{outcome}");

    // A truncated or foreign file is corrupt too.
    for junk in [&b""[..], b"\0asm\x01\0\0\0", &bytes[..20]] {
        std::fs::write(&path, junk).unwrap();
        let (_, outcome) = core(&dir).compile_traced(ADD.as_bytes()).unwrap();
        assert_eq!(outcome.origin, Origin::Compiled);
        assert!(outcome.notes[0].contains("is corrupt"), "{outcome}");
    }
}

#[tokio::test]
async fn an_entry_written_for_another_artifact_is_compiled_again_and_says_so() {
    let dir = cache_dir("mismatch");
    let engine = core(&dir);
    let cache = CompiledCache::new(&dir);
    let key = |wasm: &str| {
        ikigai_runtime::CompiledKey::new(
            ikigai_runtime::ArtifactDigest::of(wasm.as_bytes()),
            engine.compat_hash(),
        )
    };
    engine.compile_traced(SUB.as_bytes()).unwrap();
    // SUB's whole, valid entry, renamed to ADD's name.
    std::fs::rename(cache.path_of(&key(SUB)), cache.path_of(&key(ADD))).unwrap();

    let engine = core(&dir);
    let (compiled, outcome) = engine.compile_traced(ADD.as_bytes()).unwrap();
    assert_eq!(outcome.origin, Origin::Compiled);
    assert!(
        outcome.notes[0].contains("was written for another artifact or engine"),
        "{outcome}"
    );
    assert_eq!(compiled.exports(), ["add"], "never SUB's code");
    assert_eq!(add(&engine, &compiled).await, Value::I32(5));
}

#[test]
fn an_engine_with_another_compatibility_hash_never_reads_the_entry() {
    let dir = cache_dir("engine-hash");
    let fuel = core(&dir);
    let epoch = WasmtimeCoreEngine::with_metering(
        Limits::deadline(ikigai_runtime::Deadline::host(
            std::time::Duration::from_secs(60),
        )),
        Metering::EpochOnly,
    )
    .unwrap()
    .with_compiled_cache(CompiledCache::new(&dir));
    assert_ne!(fuel.compat_hash(), epoch.compat_hash());
    fuel.compile_traced(ADD.as_bytes()).unwrap();
    let (_, outcome) = epoch.compile_traced(ADD.as_bytes()).unwrap();
    assert_eq!(outcome.origin, Origin::Compiled, "{outcome}");
    assert!(
        outcome.notes.is_empty(),
        "a separate entry, not a rejected one: {outcome}"
    );
}

#[test]
fn an_unwritable_cache_still_compiles_and_says_so() {
    let dir = cache_dir("unwritable");
    std::fs::write(&dir, b"a file where the directory should be").unwrap();
    let (_, outcome) = core(&dir).compile_traced(ADD.as_bytes()).unwrap();
    assert_eq!(outcome.origin, Origin::Compiled);
    assert!(
        outcome
            .notes
            .iter()
            .any(|n| n.contains("could not be written")),
        "{outcome}"
    );
    let _ = std::fs::remove_file(&dir);
}
