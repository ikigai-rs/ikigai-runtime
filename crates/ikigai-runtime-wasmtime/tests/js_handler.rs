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
//!
//! tsrs (ledger #3659): the same handler compiled by tsrs 7.1.0-beta.tsrs1, a Rust port of the
//! TypeScript 7.1 beta compiler, through the `fixtures/ts-tsrs` fixture, built by cargo here (no
//! tool to install): as the TypeScript-to-JavaScript step, as a type check against the
//! declarations generated from the ABI (`fixtures/js-plugin/src/ikigai.d.ts`), and as a J2
//! session module that compiles TypeScript handed to it inside the sandbox.
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

/// The ABI vectors (ikigai-module 0.6.0's `tests/vectors/module-abi.txt`, vendored).
const VECTORS: &str = include_str!("../../../fixtures/js-plugin/vectors/module-abi.txt");

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
                (Verb::Source, "urn:host:vectors") => READ_NAME,
                (_, "urn:host:echo") => READ_NAME,
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
            if target == "urn:host:echo" {
                // The verb and each named argument, as the kernel received them.
                let mut echo = format!("{:?}", request.verb);
                for (name, arg) in request.args.iter() {
                    let value = match arg {
                        ArgRef::Inline(b) => format!("inline {}", String::from_utf8_lossy(b)),
                        ArgRef::Reference(iri) => format!("reference {}", iri.as_str()),
                        ArgRef::Content(id) => format!("content {id}"),
                    };
                    echo.push_str(&format!(" {name}={value}"));
                }
                return Ok(Representation::new(
                    ReprType::new("text/plain"),
                    echo.into_bytes(),
                ));
            }
            let text: &[u8] = match target.as_str() {
                "urn:host:vectors" => VECTORS.as_bytes(),
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
        "fixtures/js-plugin/src/ikigai.js",
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

/// Which compiler takes TypeScript to JavaScript.
#[derive(Clone, Copy, Debug)]
enum Compiler {
    /// esbuild 0.28.2, the pinned Go binary named by `IKIGAI_ESBUILD`.
    Esbuild,
    /// tsrs 7.1.0-beta.tsrs1, linked into `fixtures/ts-tsrs`'s native driver.
    Tsrs,
}

/// Derive the wasm module from a TypeScript source with esbuild, through a cache under `cache`.
fn transrept(ts: &Path, cache: &Path) -> (Vec<u8>, Steps) {
    transrept_with(Compiler::Esbuild, ts, cache)
}

/// Derive the wasm module from a TypeScript source with `compiler`, through a cache under `cache`.
fn transrept_with(compiler: Compiler, ts: &Path, cache: &Path) -> (Vec<u8>, Steps) {
    let mut steps = Steps::default();
    std::fs::create_dir_all(cache).unwrap();

    // TypeScript -> JavaScript, keyed by the TypeScript's digest and the compiler. Whitespace and
    // comments are dropped, so an edit that only changes types or comments yields the same
    // JavaScript.
    let ts_digest = blake3::hash(&std::fs::read(ts).unwrap());
    let js = cache.join(format!("{}.{compiler:?}.js", ts_digest.to_hex()));
    if !js.exists() {
        let status = match compiler {
            Compiler::Esbuild => Command::new(tool("IKIGAI_ESBUILD"))
                .arg(ts)
                .args(["--format=esm", "--minify-whitespace", "--log-level=warning"])
                .arg(format!("--outfile={}", js.display()))
                .status()
                .expect("esbuild runs"),
            Compiler::Tsrs => Command::new(tsrs_driver())
                .arg("emit")
                .arg(ts)
                .arg(&js)
                .stdout(std::process::Stdio::null())
                .status()
                .expect("ts-tsrs runs"),
        };
        assert!(status.success(), "{compiler:?} failed on {}", ts.display());
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

/// Build `fixtures/ts-tsrs` once per test binary, natively (the `ts-tsrs` driver) and for
/// wasm32-wasip1 (the J2 session module), in its own target directory.
fn ts_tsrs_build(target: Option<&str>) -> PathBuf {
    let target_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ts-tsrs-target");
    let mut command = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    command
        .args(["build", "--release", "--manifest-path"])
        .arg(workspace().join("fixtures/ts-tsrs/Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        .env_remove("CARGO_TARGET_DIR");
    let built = match target {
        None => {
            command.args(["--bin", "ts-tsrs"]);
            target_dir.join("release/ts-tsrs")
        }
        Some(target) => {
            command.args(["--lib", "--target", target]);
            target_dir.join(target).join("release/ts_tsrs.wasm")
        }
    };
    assert!(
        command.status().expect("cargo runs").success(),
        "building ts-tsrs failed"
    );
    built
}

/// The native tsrs driver, built once per test binary.
fn tsrs_driver() -> &'static Path {
    static DRIVER: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DRIVER.get_or_init(|| ts_tsrs_build(None))
}

/// tsrs as a J2 session module, built once per test binary.
fn tsrs_module() -> &'static [u8] {
    static MODULE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    MODULE.get_or_init(|| std::fs::read(ts_tsrs_build(Some("wasm32-wasip1"))).unwrap())
}

/// The handler's module with tsrs as its TypeScript step, built once per test binary.
fn tsrs_handler_module() -> &'static [u8] {
    static MODULE: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    MODULE.get_or_init(|| {
        let ts = workspace().join("fixtures/js-handler/handler.ts");
        let cache = Path::new(env!("CARGO_TARGET_TMPDIR")).join("js-transreption");
        transrept_with(Compiler::Tsrs, &ts, &cache).0
    })
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
async fn the_generated_surface_speaks_all_five_verbs_with_named_arguments() {
    let engine = engine();
    let compiled = engine
        .compile(handler_module())
        .expect("the module compiles");
    let kernel = Arc::new(Kernel::default());
    let reply = call(
        &engine,
        &compiled,
        "urn:js:verbs",
        grant(&[READ_NAME]),
        Arc::clone(&kernel),
    )
    .await;
    assert_eq!(
        text(reply),
        "Source a=inline 1|Sink body=inline hi|Exists r=reference urn:host:name\
         |Delete c=content b3:0000000000000000000000000000000000000000000000000000000000000000|Meta"
    );
    let seen: Vec<Verb> = kernel.seen.lock().unwrap().iter().map(|s| s.0).collect();
    assert_eq!(
        seen,
        [
            Verb::Source,
            Verb::Sink,
            Verb::Exists,
            Verb::Delete,
            Verb::Meta
        ]
    );
}

#[tokio::test]
async fn the_generated_codec_matches_every_abi_vector_in_quickjs() {
    let engine = engine();
    let compiled = engine
        .compile(handler_module())
        .expect("the module compiles");
    let kernel = Arc::new(Kernel::default());
    let reply = call(
        &engine,
        &compiled,
        "urn:js:vectors",
        grant(&[READ_NAME]),
        kernel,
    )
    .await;
    // One per variant of ModuleCall (6) and ModuleReply (8), each re-encoded byte for byte.
    assert_eq!(text(reply), "14 vectors");
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
// tsrs (ledger #3659): emit, the publish-time type check, and compiling inside the sandbox.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn tsrs_javascript_runs_unchanged_in_the_plugin() {
    let engine = engine();
    let compiled = engine
        .compile(tsrs_handler_module())
        .expect("the module compiles");
    let run = |target: &'static str, scopes: &'static [&'static str]| {
        call(
            &engine,
            &compiled,
            target,
            grant(scopes),
            Arc::new(Kernel::default()),
        )
    };
    assert_eq!(
        text(run("urn:js:greet", &[READ_NAME, WRITE_LOG]).await),
        "hello, camel"
    );
    assert_eq!(
        text(run("urn:js:vectors", &[READ_NAME]).await),
        "14 vectors"
    );
    assert_eq!(
        text(run("urn:js:verbs", &[READ_NAME]).await),
        "Source a=inline 1|Sink body=inline hi|Exists r=reference urn:host:name\
         |Delete c=content b3:0000000000000000000000000000000000000000000000000000000000000000|Meta"
    );
    assert_eq!(
        text(run("urn:js:caught", &[READ_NAME]).await),
        "Denied: urn:host:secret"
    );
    assert!(matches!(
        run("urn:js:secret", &[READ_NAME]).await,
        ModuleReply::ErrorTyped(ModuleError::Denied(_))
    ));
}

/// The publish step's check: type-check `handler` against the declarations generated from the
/// ABI; the first problem is refused as a typed `InvalidArgument` naming the file, with the
/// line, column and tsc's message as its detail.
fn publish_check(handler: &Path) -> Result<(), Error> {
    let output = Command::new(tsrs_driver())
        .arg("check")
        .arg(workspace().join("fixtures/js-plugin/src/ikigai.d.ts"))
        .arg(handler)
        .output()
        .expect("ts-tsrs runs");
    let stdout = String::from_utf8(output.stdout).unwrap();
    match stdout.lines().find_map(|l| l.strip_prefix("problem /src/")) {
        None if output.status.success() => Ok(()),
        None => panic!("ts-tsrs failed without a problem: {stdout}"),
        Some(problem) => Err(Error::InvalidArgument {
            name: problem.split(':').next().unwrap().to_string(),
            detail: problem.to_string(),
        }),
    }
}

#[test]
fn the_handler_type_checks_against_the_surface_generated_from_the_abi() {
    publish_check(&workspace().join("fixtures/js-handler/handler.ts")).expect("no problems");
}

#[test]
fn a_wrong_call_is_refused_at_publish_with_its_file_line_and_message() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ts-check");
    std::fs::create_dir_all(&dir).unwrap();
    let source =
        std::fs::read_to_string(workspace().join("fixtures/js-handler/handler.ts")).unwrap();
    let line_of = |needle: &str| source.lines().position(|l| l.contains(needle)).unwrap() + 1;
    let greet = r#"const name = ikigai.source("urn:host:name").text;"#;
    let sink = r#"ikigai.sink("urn:host:log", { body: line });"#;
    for (case, from, to, expected) in [
        (
            "missing-argument",
            greet,
            "const name = ikigai.source().text;",
            format!(
                "handler.ts:{}:23: TS2554: Expected 1-2 arguments, but got 0.",
                line_of(greet)
            ),
        ),
        (
            "wrong-argument",
            sink,
            "ikigai.sink(\"urn:host:log\", { body: 42 });",
            format!(
                "handler.ts:{}:33: TS2322: Type 'number' is not assignable to type 'Arg'.",
                line_of(sink)
            ),
        ),
        (
            "no-such-verb",
            sink,
            "ikigai.put(\"urn:host:log\", { body: line });",
            format!(
                "handler.ts:{}:10: TS2339: Property 'put' does not exist on type 'Kernel'.",
                line_of(sink)
            ),
        ),
    ] {
        let case_dir = dir.join(case);
        std::fs::create_dir_all(&case_dir).unwrap();
        let ts = case_dir.join("handler.ts");
        assert!(source.contains(from), "{case}");
        std::fs::write(&ts, source.replace(from, to)).unwrap();
        match publish_check(&ts) {
            Err(Error::InvalidArgument { name, detail }) => {
                assert_eq!(name, "handler.ts", "{case}");
                assert_eq!(detail, expected, "{case}");
            }
            other => panic!("{case}: expected a typed InvalidArgument, got {other:?}"),
        }
    }
}

/// Invoke the tsrs session module: `target` over the named source files, handed in inline.
async fn tsrs_call(
    engine: &WasmtimeCoreEngine,
    compiled: &<WasmtimeCoreEngine as CoreEngine>::Compiled,
    target: &str,
    files: &[(&str, &str)],
) -> ModuleReply {
    tsrs_call_under(
        engine,
        compiled,
        target,
        files,
        &Limits::fuel(200_000_000_000),
    )
    .await
    .map(|(reply, _)| reply)
    .expect("the call runs")
}

/// [`tsrs_call`] under `limits`, with the fuel the call consumed.
async fn tsrs_call_under(
    engine: &WasmtimeCoreEngine,
    compiled: &<WasmtimeCoreEngine as CoreEngine>::Compiled,
    target: &str,
    files: &[(&str, &str)],
    limits: &Limits,
) -> Result<(ModuleReply, Option<u64>), Error> {
    let mut request = Request::new(Verb::Source, Iri::parse(target).unwrap());
    for (name, text) in files {
        request = request.with_arg(*name, ArgRef::Inline(text.as_bytes().to_vec()));
    }
    let encoded = postcard::to_allocvec(&ModuleCall::Invoke {
        request,
        capability: grant(&[]),
    })
    .unwrap();
    let invoked = engine
        .invoke(
            compiled,
            Call::Session {
                encoded_call: &encoded,
            },
            limits,
            Arc::new(Kernel::default()),
        )
        .await?;
    match invoked.output {
        Output::Session(bytes) => Ok((
            postcard::from_bytes(&bytes).expect("a reply"),
            invoked.consumed.fuel,
        )),
        _ => panic!("expected a session output"),
    }
}

#[tokio::test]
async fn tsrs_compiles_typescript_handed_to_it_inside_the_sandbox() {
    let engine = engine();
    let compiled = engine.compile(tsrs_module()).expect("the module compiles");
    let handler =
        std::fs::read_to_string(workspace().join("fixtures/js-handler/handler.ts")).unwrap();
    let surface =
        std::fs::read_to_string(workspace().join("fixtures/js-plugin/src/ikigai.d.ts")).unwrap();

    // Emit: the same JavaScript as the native driver writes.
    let native = Path::new(env!("CARGO_TARGET_TMPDIR")).join("ts-native.js");
    let status = Command::new(tsrs_driver())
        .arg("emit")
        .arg(workspace().join("fixtures/js-handler/handler.ts"))
        .arg(&native)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let reply = tsrs_call(
        &engine,
        &compiled,
        "urn:ts:emit",
        &[("handler.ts", &handler)],
    )
    .await;
    assert_eq!(text(reply), std::fs::read_to_string(&native).unwrap());

    // Check: clean, then a wrong call refused, typed, with its file, line and message.
    let files = [("ikigai.d.ts", surface.as_str()), ("handler.ts", &handler)];
    assert_eq!(
        text(tsrs_call(&engine, &compiled, "urn:ts:check", &files).await),
        "ok"
    );
    let wrong = handler.replace(
        r#"ikigai.source("urn:host:name").text;"#,
        "ikigai.source().text;",
    );
    let files = [("ikigai.d.ts", surface.as_str()), ("handler.ts", &wrong)];
    match tsrs_call(&engine, &compiled, "urn:ts:check", &files).await {
        ModuleReply::ErrorTyped(ModuleError::InvalidArgument { name, detail }) => {
            assert_eq!(name, "handler.ts");
            assert!(
                detail.ends_with("TS2554: Expected 1-2 arguments, but got 0."),
                "{detail}"
            );
        }
        other => panic!("expected a typed InvalidArgument, got {other:?}"),
    }

    // The host's files are not there: a path that exists on every Linux host is not found.
    let probe = "/// <reference path=\"/etc/hostname\" />\nimport \"/etc/passwd\";\nexport {};\n";
    match tsrs_call(&engine, &compiled, "urn:ts:check", &[("probe.ts", probe)]).await {
        ModuleReply::ErrorTyped(ModuleError::InvalidArgument { name, detail }) => {
            assert_eq!(name, "probe.ts");
            assert!(
                detail.contains("TS6231: Could not resolve the path '/etc/hostname'"),
                "{detail}"
            );
        }
        other => panic!("expected the host's file to be missing, got {other:?}"),
    }
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
    measure_one(
        "js    urn:js:greet   (tsrs emit)",
        tsrs_handler_module(),
        "urn:js:greet",
        200,
    )
    .await;
    measure_typescript().await;
}

/// The TypeScript step (#3659): esbuild as a process per compile, tsrs linked in process
/// (timed inside the driver), and tsrs as a J2 session module.
async fn measure_typescript() {
    let handler_ts = workspace().join("fixtures/js-handler/handler.ts");
    let surface_ts = workspace().join("fixtures/js-plugin/src/ikigai.d.ts");
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("measure-esbuild.js");
    let mut esbuild = Vec::new();
    for _ in 0..21 {
        let t = Instant::now();
        let status = Command::new(tool("IKIGAI_ESBUILD"))
            .arg(&handler_ts)
            .args(["--format=esm", "--minify-whitespace", "--log-level=warning"])
            .arg(format!("--outfile={}", out.display()))
            .status()
            .unwrap();
        assert!(status.success());
        esbuild.push(t.elapsed());
    }
    eprintln!(
        "esbuild emit (a process each)       first {:>9.2?}  median of 20 more {:>9.2?}  output {} B",
        esbuild[0],
        median(esbuild[1..].to_vec()),
        std::fs::metadata(&out).unwrap().len()
    );
    let tsrs_out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("measure-tsrs.js");
    let status = Command::new(tsrs_driver())
        .arg("emit")
        .arg(&handler_ts)
        .arg(&tsrs_out)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let timed = Command::new(tsrs_driver())
        .arg("time")
        .arg(&surface_ts)
        .arg(&handler_ts)
        .arg("20")
        .output()
        .unwrap();
    for line in String::from_utf8(timed.stdout).unwrap().lines() {
        eprintln!("tsrs in process     {line}");
    }
    eprintln!(
        "tsrs emit output {} B; driver binary {} B",
        std::fs::metadata(&tsrs_out).unwrap().len(),
        std::fs::metadata(tsrs_driver()).unwrap().len()
    );

    // In the sandbox: compile the module (no compiled cache), then each call in a fresh instance.
    let handler = std::fs::read_to_string(&handler_ts).unwrap();
    let surface = std::fs::read_to_string(&surface_ts).unwrap();
    let wasm = tsrs_module();
    let started = Instant::now();
    let engine = engine();
    let compiled = engine.compile(wasm).expect("compiles");
    let compiled_at = started.elapsed();
    let emit = [("handler.ts", handler.as_str())];
    let check = [("ikigai.d.ts", surface.as_str()), ("handler.ts", &handler)];
    let reply = tsrs_call(&engine, &compiled, "urn:ts:emit", &emit).await;
    let cold = started.elapsed();
    assert!(matches!(reply, ModuleReply::Resolved(_)), "{reply:?}");
    for (label, target, files) in [
        ("emit", "urn:ts:emit", &emit[..]),
        ("check", "urn:ts:check", &check[..]),
    ] {
        let mut warm = Vec::new();
        for _ in 0..10 {
            let t = Instant::now();
            let reply = tsrs_call(&engine, &compiled, target, files).await;
            assert!(matches!(reply, ModuleReply::Resolved(_)), "{reply:?}");
            warm.push(t.elapsed());
        }
        let (_, fuel) = tsrs_call_under(
            &engine,
            &compiled,
            target,
            files,
            &Limits::fuel(200_000_000_000),
        )
        .await
        .unwrap();
        eprintln!(
            "tsrs in the sandbox {label:<5} size {:>9} B  compile {:>9.2?}  cold {:>9.2?}  per call median {:>9.2?}  fuel {}",
            wasm.len(),
            compiled_at,
            cold,
            median(warm),
            fuel.unwrap_or_default()
        );
    }
    // The smallest linear-memory budget, in steps of 16 MiB from the 80 MiB shadow stack alone,
    // a check fits in.
    let mut mib = 80;
    loop {
        let limits = Limits {
            memory_bytes: Some(mib << 20),
            ..Limits::fuel(200_000_000_000)
        };
        let fits = matches!(
            tsrs_call_under(&engine, &compiled, "urn:ts:check", &check, &limits).await,
            Ok((ModuleReply::Resolved(_), _))
        );
        if fits {
            eprintln!(
                "tsrs in the sandbox check fits a memory budget of {mib} MiB, not {} MiB",
                mib - 16
            );
            break;
        }
        mib += 16;
        assert!(mib <= 1024, "no budget up to 1 GiB fits");
    }
}
