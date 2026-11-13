//! A J2 session module that ALSO imports `wasi_snapshot_preview1` (R-D6 point 2, amended
//! 2026-11-12). Always-on WAT reproductions, no wasm toolchain: before the amendment such a
//! module failed to instantiate (`unknown import: wasi_snapshot_preview1::fd_write`), which is
//! what a JavaScript engine compiled to wasm (Javy's QuickJS imports ten p1 functions) hits.
//!
//! The rules each test pins:
//! - the module instantiates and its `host_call` reaches the kernel, held to the grant;
//! - stdout and stderr go to a bounded capture, never the host's stdio; past the bound a write
//!   is dropped and counted; stdin reads EOF;
//! - `proc_exit` ends the CALL with an endpoint error naming the code, never the host;
//! - the empty context grants nothing: no preopen to open a path under, no socket, no env or
//!   args, a fixed clock.

use std::sync::{Arc, Mutex};

use ikigai_core::{Iri, ReprType, Request, Verb};
use ikigai_module::{ModuleCall, ModuleError, ModuleReply};
use ikigai_runtime::{
    Call, Capability, CoreEngine, Error, HostFuture, HostResolver, Limits, Output, Representation,
};
use ikigai_runtime_wasmtime::{SessionStdio, WasmtimeCoreEngine, SESSION_STDIO_CAPACITY};

const READ_NAME: &str = "urn:cap:host:read:name";

/// The kernel stand-in: serves `urn:host:name` to a capability that allows reading it.
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
            Ok(Representation::new(
                ReprType::new("text/plain"),
                b"camel".to_vec(),
            ))
        })
    }
}

type Seen = Arc<Mutex<Vec<SessionStdio>>>;

fn engine() -> (WasmtimeCoreEngine, Seen) {
    let seen: Seen = Arc::default();
    let sink = Arc::clone(&seen);
    let engine = WasmtimeCoreEngine::new(Limits::fuel(u64::MAX))
        .expect("a bounded ceiling")
        .with_session_stdio(Arc::new(move |stdio: &SessionStdio| {
            sink.lock().unwrap().push(stdio.clone())
        }));
    (engine, seen)
}

/// The J2 exports and the `host_call` import every module below shares, plus the p1 imports it
/// names, with `body` as `ikigai_invoke`'s body and `data` as extra segments.
fn module(p1_imports: &str, data: &str, body: &str) -> String {
    format!(
        r#"(module
  (import "ikigai" "host_call" (func $host_call (param i32 i32) (result i64)))
  {p1_imports}
  (memory (export "memory") 2)
  (global $heap (mut i32) (i32.const 8192))
  (func (export "ikigai_alloc") (param $n i32) (result i32) (local $p i32)
    global.get $heap local.set $p
    global.get $heap local.get $n i32.add global.set $heap
    local.get $p)
  (func (export "ikigai_free") (param i32 i32))
  {data}
  (func (export "ikigai_invoke") (param i32 i32) (result i64) {body}))"#
    )
}

const FD_WRITE: &str = r#"(import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))"#;

/// WAT data-string escapes for `bytes`.
fn escaped(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("\\{b:02x}")).collect()
}

/// The guest's `HostCall` for `urn:host:name`, claiming root (the host clamps the claim).
fn host_call_bytes() -> Vec<u8> {
    postcard::to_allocvec(&ModuleReply::HostCall {
        request: Request::new(Verb::Source, Iri::parse("urn:host:name").unwrap()),
        capability: Capability::root(),
    })
    .unwrap()
}

fn invoke_call(capability: Capability) -> Vec<u8> {
    postcard::to_allocvec(&ModuleCall::Invoke {
        request: Request::new(Verb::Source, Iri::parse("urn:example:handler").unwrap()),
        capability,
    })
    .unwrap()
}

async fn run(
    engine: &WasmtimeCoreEngine,
    wat: &str,
    capability: Capability,
) -> Result<Vec<u8>, Error> {
    let compiled = engine.compile(wat.as_bytes()).expect("the WAT compiles");
    let encoded = invoke_call(capability);
    let invoked = engine
        .invoke(
            &compiled,
            Call::Session {
                encoded_call: &encoded,
            },
            &Limits::fuel(100_000_000),
            Arc::new(HostName),
        )
        .await?;
    match invoked.output {
        Output::Session(bytes) => Ok(bytes),
        _ => panic!("expected a session output"),
    }
}

fn allowing() -> Capability {
    Capability::root().attenuate([READ_NAME.to_string()])
}

fn empty() -> Capability {
    Capability::root().attenuate(std::iter::empty::<String>())
}

/// Writes `hello out` to stdout and `hello err` to stderr, then makes one `host_call` and
/// returns the host's answer as its reply.
fn greeter() -> String {
    let call = host_call_bytes();
    let data = format!(
        r#"(data (i32.const 0) "\00\02\00\00\09\00\00\00")
  (data (i32.const 8) "\10\02\00\00\09\00\00\00")
  (data (i32.const 512) "hello out")
  (data (i32.const 528) "hello err")
  (data (i32.const 1024) "{}")"#,
        escaped(&call)
    );
    let body = format!(
        "(drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 16)))
         (drop (call $fd_write (i32.const 2) (i32.const 8) (i32.const 1) (i32.const 16)))
         (call $host_call (i32.const 1024) (i32.const {}))",
        call.len()
    );
    module(FD_WRITE, &data, &body)
}

#[tokio::test]
async fn a_session_module_importing_wasi_p1_instantiates_and_its_host_call_reaches_the_kernel() {
    let (engine, _) = engine();
    let reply = run(&engine, &greeter(), allowing())
        .await
        .expect("the call runs");
    match postcard::from_bytes::<ModuleCall>(&reply).unwrap() {
        ModuleCall::HostResult(Ok(representation)) => assert_eq!(representation.bytes, b"camel"),
        other => panic!("expected the host name, got {other:?}"),
    }
}

#[tokio::test]
async fn host_call_stays_held_to_the_grant_when_wasi_is_linked() {
    let (engine, _) = engine();
    // The guest claims root; the session grants nothing, so the kernel refuses, typed.
    let reply = run(&engine, &greeter(), empty())
        .await
        .expect("the call runs");
    match postcard::from_bytes::<ModuleCall>(&reply).unwrap() {
        ModuleCall::HostError(ModuleError::Denied { .. }) => {}
        other => panic!("expected a typed Denied, got {other:?}"),
    }
}

#[tokio::test]
async fn stdout_and_stderr_go_to_the_capture_never_the_reply() {
    let (engine, seen) = engine();
    let reply = run(&engine, &greeter(), allowing()).await.unwrap();
    assert!(!reply.windows(9).any(|w| w == b"hello out"));
    let seen = seen.lock().unwrap();
    let stdio = seen.last().expect("the host saw the capture");
    assert_eq!(stdio.stdout, b"hello out");
    assert_eq!(stdio.stderr, b"hello err");
    assert_eq!((stdio.stdout_dropped, stdio.stderr_dropped), (0, 0));
}

#[tokio::test]
async fn past_the_bound_writes_are_dropped_and_counted_and_the_call_still_answers() {
    let (engine, seen) = engine();
    // 4096-byte writes to stdout, 64 of them: 256 KiB, four times the bound.
    let data = r#"(data (i32.const 0) "\00\10\00\00\00\10\00\00")"#;
    let body = r#"(local $i i32)
        (loop $l
          (drop (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1) (i32.const 16)))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br_if $l (i32.lt_u (local.get $i) (i32.const 64))))
        (i64.const 0)"#;
    run(&engine, &module(FD_WRITE, data, body), allowing())
        .await
        .expect("a full capture never fails the call");
    let seen = seen.lock().unwrap();
    let stdio = seen.last().unwrap();
    assert_eq!(stdio.stdout.len(), SESSION_STDIO_CAPACITY);
    assert_eq!(
        stdio.stdout_dropped,
        (64 * 4096 - SESSION_STDIO_CAPACITY) as u64
    );
}

#[tokio::test]
async fn stdin_reads_eof() {
    let (engine, _) = engine();
    let imports = r#"(import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))"#;
    // iovec at 0 → 64 bytes at 512; errno at 32, bytes read at 36.
    let data = r#"(data (i32.const 0) "\00\02\00\00\40\00\00\00")"#;
    let body = r#"(i32.store (i32.const 32)
          (call $fd_read (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 36)))
        (i64.or (i64.shl (i64.const 32) (i64.const 32)) (i64.const 8))"#;
    let reply = run(&engine, &module(imports, data, body), allowing())
        .await
        .unwrap();
    assert_eq!(reply, [0, 0, 0, 0, 0, 0, 0, 0], "errno 0, zero bytes read");
}

#[tokio::test]
async fn proc_exit_ends_the_call_not_the_host() {
    let (engine, _) = engine();
    let imports = r#"(import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))"#;
    let body = "(call $proc_exit (i32.const 7)) (i64.const 0)";
    let err = run(&engine, &module(imports, "", body), allowing())
        .await
        .expect_err("proc_exit ends the call");
    match err {
        Error::Endpoint(message) => assert!(message.contains("code 7"), "{message}"),
        other => panic!("expected an endpoint error, got {other:?}"),
    }
    // Even exit code 0 has no reply to give: it ends the call too.
    let body = "(call $proc_exit (i32.const 0)) (i64.const 0)";
    let err = run(&engine, &module(imports, "", body), allowing())
        .await
        .expect_err("proc_exit(0) ends the call");
    assert!(
        matches!(err, Error::Endpoint(ref m) if m.contains("code 0")),
        "{err:?}"
    );
    // The host, and the same engine, answer the next call.
    let reply = run(&engine, &greeter(), allowing()).await.unwrap();
    assert!(matches!(
        postcard::from_bytes::<ModuleCall>(&reply).unwrap(),
        ModuleCall::HostResult(Ok(_))
    ));
}

#[tokio::test]
async fn the_empty_context_offers_nothing_to_open_and_no_authority() {
    let (engine, _) = engine();
    let imports = r#"
  (import "wasi_snapshot_preview1" "path_open" (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_prestat_get" (func $fd_prestat_get (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "sock_accept" (func $sock_accept (param i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "environ_sizes_get" (func $environ_sizes_get (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "args_sizes_get" (func $args_sizes_get (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "clock_time_get" (func $clock_time_get (param i32 i64 i32) (result i32)))"#;
    let data = r#"(data (i32.const 512) "etc/passwd")"#;
    // Reply: errno of path_open, fd_prestat_get, sock_accept (each i32), the env count and the
    // arg count (i32), four bytes of padding, then the realtime clock (u64): 32 bytes at 32.
    let body = r#"
        (i32.store (i32.const 32) (call $path_open (i32.const 3) (i32.const 0) (i32.const 512)
          (i32.const 10) (i32.const 0) (i64.const 0x1fffffff) (i64.const 0x1fffffff) (i32.const 0) (i32.const 256)))
        (i32.store (i32.const 36) (call $fd_prestat_get (i32.const 3) (i32.const 256)))
        (i32.store (i32.const 40) (call $sock_accept (i32.const 3) (i32.const 0) (i32.const 256)))
        (drop (call $environ_sizes_get (i32.const 44) (i32.const 256)))
        (drop (call $args_sizes_get (i32.const 48) (i32.const 256)))
        (drop (call $clock_time_get (i32.const 0) (i64.const 1) (i32.const 56)))
        (i64.or (i64.shl (i64.const 32) (i64.const 32)) (i64.const 32))"#;
    let reply = run(&engine, &module(imports, data, body), allowing())
        .await
        .unwrap();
    let word = |i: usize| u32::from_le_bytes(reply[i..i + 4].try_into().unwrap());
    const EBADF: u32 = 8;
    assert_eq!(word(0), EBADF, "path_open: no preopen to open under");
    assert_eq!(word(4), EBADF, "fd_prestat_get: no preopens at all");
    assert_eq!(word(8), EBADF, "sock_accept: no socket");
    assert_eq!((word(12), word(16)), (0, 0), "no env, no args");
    assert_eq!(
        u64::from_le_bytes(reply[24..32].try_into().unwrap()),
        0,
        "the fixed clock"
    );
}
