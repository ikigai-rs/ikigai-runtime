//! Spike (#3628): a TypeScript handler run as a JavaScript engine module (Javy's QuickJS) inside
//! the core-module tier, reaching the kernel only through the verbs its grant allows.
//!
//! Behind the `js-fixtures` feature, which needs two pinned, checksum-verified tools named by
//! the environment: `IKIGAI_JAVY` (the javy CLI, for `init-plugin`) and `IKIGAI_ESBUILD` (for
//! TypeScript to JavaScript): javy 9.1.0 and esbuild 0.28.2, each checked against its published
//! digest before use.
//!
//! The build is a TRANSREPTION: `fixtures/js-handler/handler.ts` is the resource; the
//! JavaScript and the pre-initialized wasm module are representations derived from it, each
//! cached under the digest of what it was derived from (`transrept`), so an edit recompiles
//! exactly what depends on it.
#![cfg(feature = "js-fixtures")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ikigai_core::{ArgRef, Iri, ReprType, Request, Verb};
use ikigai_module::{ModuleCall, ModuleError, ModuleReply};
use ikigai_runtime::{
    Call, Capability, CoreEngine, Error, HostFuture, HostResolver, Limits, Output, Representation,
};
use ikigai_runtime_wasmtime::WasmtimeCoreEngine;

const JAIL_GUEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/jail_guest.wasm"));

const READ_NAME: &str = "urn:cap:host:read:name";
const WRITE_LOG: &str = "urn:cap:host:write:log";
const READ_SECRET: &str = "urn:cap:host:read:secret";

/// The kernel stand-in. It serves three resources, each to a capability that allows it, and
/// records every request that reaches it, so a test can tell a grant refusal (the request
/// arrived and was refused here) from an engine failure (it never arrived).
#[derive(Default)]
struct Kernel {
    seen: Mutex<Vec<(Verb, String, bool)>>,
    log: Mutex<Vec<Vec<u8>>>,
}

impl HostResolver for Kernel {
    fn resolve<'a>(&'a self, request: Request, capability: &'a Capability) -> HostFuture<'a> {
        Box::pin(async move {
            let target = request.target.as_str().to_string();
            let scope = match (request.verb, target.as_str()) {
                (Verb::Source, "urn:host:name") => READ_NAME,
                (Verb::Sink, "urn:host:log") => WRITE_LOG,
                (Verb::Source, "urn:host:secret") => READ_SECRET,
                _ => return Err(Error::NotFound(target)),
            };
            let allowed = capability.allows(scope);
            self.seen
                .lock()
                .unwrap()
                .push((request.verb, target.clone(), allowed));
            if !allowed {
                return Err(Error::Denied(target));
            }
            let text: &[u8] = match target.as_str() {
                "urn:host:name" => b"camel",
                "urn:host:secret" => b"hunter2",
                _ => {
                    let Some(ArgRef::Inline(body)) = request.args.get("body") else {
                        return Err(Error::Endpoint("a Sink needs a body".into()));
                    };
                    self.log.lock().unwrap().push(body.clone());
                    b""
                }
            };
            Ok(Representation::new(
                ReprType::new("text/plain"),
                text.to_vec(),
            ))
        })
    }
}

fn grant(scopes: &[&str]) -> Capability {
    Capability::root().attenuate(scopes.iter().map(|s| s.to_string()))
}

// ---------------------------------------------------------------------------------------------
// The transreption: handler.ts -> handler.js -> handler.wasm, each cached under its input's digest.
// ---------------------------------------------------------------------------------------------

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn tool(var: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(var).unwrap_or_else(|| {
        panic!("{var} must name the tool: javy 9.1.0 or esbuild 0.28.2, checksum-verified")
    }))
}

/// What a transreption did: which representations it derived and which it found cached.
#[derive(Debug, Default, PartialEq, Eq)]
struct Steps {
    javascript_built: bool,
    module_built: bool,
}

/// Everything the JavaScript-to-wasm step depends on besides the JavaScript: the plugin's
/// source and manifest, the guest SDK and the tool versions. An edit to any of them is a new
/// module.
fn plugin_digest() -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    let root = workspace();
    for file in [
        "fixtures/js-plugin/Cargo.toml",
        "fixtures/js-plugin/src/lib.rs",
        "crates/ikigai-runtime-guest/src/core_module.rs",
        "crates/ikigai-runtime-guest/Cargo.toml",
    ] {
        h.update(&std::fs::read(root.join(file)).unwrap());
    }
    for var in ["IKIGAI_JAVY", "IKIGAI_ESBUILD"] {
        h.update(tool(var).to_string_lossy().as_bytes());
    }
    h.finalize()
}

/// Derive the wasm module from a TypeScript source, through a cache under `cache`.
fn transrept(ts: &Path, cache: &Path) -> (Vec<u8>, Steps) {
    let mut steps = Steps::default();
    std::fs::create_dir_all(cache).unwrap();

    // TypeScript -> JavaScript, keyed by the TypeScript's digest. Whitespace and comments are
    // dropped, so an edit that only changes types or comments yields the same JavaScript.
    let ts_digest = blake3::hash(&std::fs::read(ts).unwrap());
    let js = cache.join(format!("{}.js", ts_digest.to_hex()));
    if !js.exists() {
        let status = Command::new(tool("IKIGAI_ESBUILD"))
            .arg(ts)
            .args(["--format=esm", "--minify-whitespace", "--log-level=warning"])
            .arg(format!("--outfile={}", js.display()))
            .status()
            .expect("esbuild runs");
        assert!(status.success(), "esbuild failed on {}", ts.display());
        steps.javascript_built = true;
    }

    // JavaScript -> wasm, keyed by the JavaScript's digest and the plugin's.
    let mut key = blake3::Hasher::new();
    key.update(&std::fs::read(&js).unwrap());
    key.update(plugin_digest().as_bytes());
    let wasm = cache.join(format!("{}.wasm", key.finalize().to_hex()));
    if !wasm.exists() {
        let target_dir = cache.join("plugin-target");
        let status = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args([
                "build",
                "--release",
                "--target",
                "wasm32-wasip1",
                "--manifest-path",
            ])
            .arg(workspace().join("fixtures/js-plugin/Cargo.toml"))
            .arg("--target-dir")
            .arg(&target_dir)
            .env("IKIGAI_JS_HANDLER", &js)
            .env_remove("CARGO_TARGET_DIR")
            .status()
            .expect("cargo runs");
        assert!(status.success(), "building the plugin failed");
        let built = target_dir.join("wasm32-wasip1/release/js_plugin.wasm");
        let status = Command::new(tool("IKIGAI_JAVY"))
            .arg("init-plugin")
            .arg("--deterministic")
            .arg(&built)
            .arg("-o")
            .arg(&wasm)
            .status()
            .expect("javy runs");
        assert!(status.success(), "javy init-plugin failed");
        steps.module_built = true;
    }
    (std::fs::read(&wasm).unwrap(), steps)
}

/// The handler's module, built once per test binary.
fn handler_module() -> &'static [u8] {
    static MODULE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    MODULE.get_or_init(|| {
        let ts = workspace().join("fixtures/js-handler/handler.ts");
        let cache = Path::new(env!("CARGO_TARGET_TMPDIR")).join("js-transreption");
        transrept(&ts, &cache).0
    })
}

// ---------------------------------------------------------------------------------------------
// Running it.
// ---------------------------------------------------------------------------------------------

fn engine() -> WasmtimeCoreEngine {
    WasmtimeCoreEngine::new(Limits::fuel(u64::MAX)).expect("a bounded ceiling")
}

fn invoke_bytes(target: &str, capability: Capability) -> Vec<u8> {
    postcard::to_allocvec(&ModuleCall::Invoke {
        request: Request::new(Verb::Source, Iri::parse(target).unwrap()),
        capability,
    })
    .unwrap()
}

async fn call(
    engine: &WasmtimeCoreEngine,
    compiled: &<WasmtimeCoreEngine as CoreEngine>::Compiled,
    target: &str,
    capability: Capability,
    kernel: Arc<Kernel>,
) -> ModuleReply {
    let encoded = invoke_bytes(target, capability);
    let invoked = engine
        .invoke(
            compiled,
            Call::Session {
                encoded_call: &encoded,
            },
            &Limits::fuel(20_000_000_000),
            kernel,
        )
        .await
        .expect("the call runs");
    match invoked.output {
        Output::Session(bytes) => postcard::from_bytes(&bytes).expect("a reply"),
        _ => panic!("expected a session output"),
    }
}

fn text(reply: ModuleReply) -> String {
    match reply {
        ModuleReply::Resolved(r) => String::from_utf8(r.bytes).unwrap(),
        other => panic!("expected a representation, got {other:?}"),
    }
}

#[tokio::test]
async fn the_handler_sources_and_sinks_what_its_grant_allows() {
    let engine = engine();
    let compiled = engine
        .compile(handler_module())
        .expect("the module compiles");
    let kernel = Arc::new(Kernel::default());
    let reply = call(
        &engine,
        &compiled,
        "urn:js:greet",
        grant(&[READ_NAME, WRITE_LOG]),
        Arc::clone(&kernel),
    )
    .await;
    assert_eq!(text(reply), "hello, camel");
    assert_eq!(*kernel.log.lock().unwrap(), vec![b"hello, camel".to_vec()]);
    assert_eq!(
        *kernel.seen.lock().unwrap(),
        vec![
            (Verb::Source, "urn:host:name".into(), true),
            (Verb::Sink, "urn:host:log".into(), true)
        ]
    );
}

#[tokio::test]
async fn a_resource_outside_the_grant_is_refused_typed_by_the_grant_not_the_engine() {
    let engine = engine();
    let compiled = engine
        .compile(handler_module())
        .expect("the module compiles");

    // Without the scope: the request reaches the kernel, the kernel refuses it, and the
    // refusal comes back with its type and resource.
    let kernel = Arc::new(Kernel::default());
    let reply = call(
        &engine,
        &compiled,
        "urn:js:secret",
        grant(&[READ_NAME, WRITE_LOG]),
        Arc::clone(&kernel),
    )
    .await;
    match reply {
        ModuleReply::ErrorTyped(ModuleError::Denied(resource)) => {
            assert_eq!(resource, "urn:host:secret")
        }
        other => panic!("expected a typed Denied, got {other:?}"),
    }
    assert_eq!(
        *kernel.seen.lock().unwrap(),
        vec![(Verb::Source, "urn:host:secret".into(), false)],
        "the refusal was the kernel's, under the session's grant"
    );

    // JavaScript sees the class and the resource.
    let reply = call(
        &engine,
        &compiled,
        "urn:js:caught",
        grant(&[READ_NAME]),
        Arc::new(Kernel::default()),
    )
    .await;
    assert_eq!(text(reply), "Denied: urn:host:secret");

    // The same module and engine, granted the scope: the same code reads it. Nothing in the
    // engine refused it before.
    let reply = call(
        &engine,
        &compiled,
        "urn:js:secret",
        grant(&[READ_SECRET]),
        Arc::new(Kernel::default()),
    )
    .await;
    assert_eq!(text(reply), "hunter2");
}

#[tokio::test]
async fn an_exception_the_kernel_did_not_throw_is_an_endpoint_error() {
    let engine = engine();
    let compiled = engine
        .compile(handler_module())
        .expect("the module compiles");
    let reply = call(
        &engine,
        &compiled,
        "urn:js:nope",
        grant(&[]),
        Arc::new(Kernel::default()),
    )
    .await;
    match reply {
        ModuleReply::Error(message) => {
            assert!(
                message.contains("no such endpoint: urn:js:nope"),
                "{message}"
            )
        }
        other => panic!("expected an endpoint error, got {other:?}"),
    }
}

#[test]
fn an_edit_recompiles_exactly_what_depends_on_it() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("js-transreption-edit");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cache = dir.join("cache");
    let ts = dir.join("handler.ts");
    let source =
        std::fs::read_to_string(workspace().join("fixtures/js-handler/handler.ts")).unwrap();
    std::fs::write(&ts, &source).unwrap();

    let (first, steps) = transrept(&ts, &cache);
    assert_eq!(
        steps,
        Steps {
            javascript_built: true,
            module_built: true
        }
    );
    // Unchanged: both representations come from the cache.
    let (again, steps) = transrept(&ts, &cache);
    assert_eq!(steps, Steps::default());
    assert_eq!(first, again);

    // A type-only edit is a new TypeScript resource, so its JavaScript is derived again; that
    // JavaScript is identical, so the module is not.
    std::fs::write(
        &ts,
        source.replace("rounds: number", "rounds: 1000000 | number"),
    )
    .unwrap();
    let (typed, steps) = transrept(&ts, &cache);
    assert_eq!(
        steps,
        Steps {
            javascript_built: true,
            module_built: false
        }
    );
    assert_eq!(first, typed);

    // An edit to behavior derives both again.
    std::fs::write(&ts, source.replace("hello, ${name}", "hi, ${name}")).unwrap();
    let (edited, steps) = transrept(&ts, &cache);
    assert_eq!(
        steps,
        Steps {
            javascript_built: true,
            module_built: true
        }
    );
    assert_ne!(first, edited);
}

// ---------------------------------------------------------------------------------------------
// Measurements (`cargo test ... -- --ignored --nocapture measure`).
// ---------------------------------------------------------------------------------------------

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

async fn measure_one(name: &str, wasm: &[u8], target: &str, rounds: usize) {
    let scopes = grant(&[READ_NAME, WRITE_LOG]);
    // Cold: compile (no cache) and the first call.
    let started = Instant::now();
    let engine = engine();
    let compiled = engine.compile(wasm).expect("compiles");
    let compiled_at = started.elapsed();
    let reply = call(&engine, &compiled, target, scopes.clone(), Arc::default()).await;
    let cold = started.elapsed();
    assert!(
        matches!(reply, ModuleReply::Resolved(_)),
        "{name}: {reply:?}"
    );
    // Warm: the module compiled, a fresh instance per call (as the tier always runs).
    let mut warm = Vec::new();
    for _ in 0..rounds {
        let t = Instant::now();
        call(&engine, &compiled, target, scopes.clone(), Arc::default()).await;
        warm.push(t.elapsed());
    }
    eprintln!(
        "{name:<34} size {:>9} B  compile {:>9.2?}  cold {:>9.2?}  warm median {:>9.2?}",
        wasm.len(),
        compiled_at,
        cold,
        median(warm)
    );
}

#[tokio::test]
#[ignore = "a measurement, run on demand"]
async fn measure() {
    let js = handler_module();
    measure_one(
        "rust  urn:jail:hello (1 Source)",
        JAIL_GUEST,
        "urn:jail:hello",
        200,
    )
    .await;
    measure_one("js    urn:js:hello   (1 Source)", js, "urn:js:hello", 200).await;
    measure_one(
        "js    urn:js:greet   (Source+Sink)",
        js,
        "urn:js:greet",
        200,
    )
    .await;
    measure_one("js    urn:js:loop    (1e6 iters)", js, "urn:js:loop", 5).await;
}
