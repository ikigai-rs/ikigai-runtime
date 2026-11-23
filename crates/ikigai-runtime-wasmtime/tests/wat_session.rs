//! Always-on, hermetic tests of the J2 session boundary against MALFORMED guests: hand-built WAT
//! modules that hand the host pointers and lengths no well-behaved guest would. Each is a clear
//! refusal (an `Err` for the call), never a panic or a host abort, and the host answers the next
//! call.

use std::sync::Arc;

use ikigai_runtime::session::pack;
use ikigai_runtime::space::Convention;
use ikigai_runtime::{Call, CoreEngine, Error, Limits, NoHost, Output};
use ikigai_runtime_wasmtime::{detect_convention, WasmtimeCoreEngine};

fn engine() -> WasmtimeCoreEngine {
    WasmtimeCoreEngine::new(Limits::fuel(u64::MAX)).expect("a bounded ceiling")
}

/// A one-page (64 KiB) session guest whose `ikigai_alloc` answers `alloc` and whose
/// `ikigai_invoke` runs `body` (which leaves the packed reply on the stack).
fn guest(alloc: u32, body: &str) -> String {
    format!(
        r#"(module
            (import "ikigai" "host_call" (func $host_call (param i32 i32) (result i64)))
            (memory (export "memory") 1)
            (func (export "ikigai_alloc") (param i32) (result i32) i32.const {alloc})
            (func (export "ikigai_free") (param i32 i32))
            (func (export "ikigai_invoke") (param i32 i32) (result i64) {body}))"#,
        alloc = alloc as i32
    )
}

async fn run(wat: &str) -> Result<Vec<u8>, Error> {
    let engine = engine();
    let compiled = engine.compile(wat.as_bytes()).expect("the WAT compiles");
    assert_eq!(detect_convention(&compiled), Convention::Session);
    let invoked = engine
        .invoke(
            &compiled,
            Call::Session {
                encoded_call: b"\x00\x01\x02",
            },
            &Limits::fuel(10_000_000),
            Arc::new(NoHost),
        )
        .await?;
    match invoked.output {
        Output::Session(bytes) => Ok(bytes),
        _ => panic!("expected a session output"),
    }
}

fn packed(ptr: u32, len: u32) -> String {
    format!("i64.const {}", pack(ptr, len) as i64)
}

#[tokio::test]
async fn a_well_formed_reply_is_read_back() {
    // The host wrote the call at 16; the guest answers it as its own reply.
    let reply = run(&guest(16, &packed(16, 3))).await.unwrap();
    assert_eq!(reply, b"\x00\x01\x02");
}

#[tokio::test]
async fn a_reply_out_of_bounds_or_overflowing_is_refused() {
    for (ptr, len) in [
        (65_536, 1),          // one past the end
        (65_530, 7),          // straddles the end
        (0, 65_537),          // a length larger than guest memory
        (u32::MAX, u32::MAX), // ptr + len overflows u32
        (u32::MAX - 1, 4),    // ptr + len wraps
    ] {
        let err = run(&guest(16, &packed(ptr, len))).await.unwrap_err();
        assert!(
            matches!(&err, Error::Endpoint(m) if m.contains("out of bounds") || m.contains("overflow")),
            "({ptr}, {len}): {err:?}"
        );
    }
}

#[tokio::test]
async fn an_alloc_pointer_outside_memory_is_refused() {
    for alloc in [65_535, 70_000, u32::MAX] {
        let err = run(&guest(alloc, &packed(0, 0))).await.unwrap_err();
        assert!(matches!(err, Error::Endpoint(_)), "{alloc}: {err:?}");
    }
}

#[tokio::test]
async fn a_host_call_out_of_bounds_or_overflowing_is_a_trap_never_a_panic() {
    for (ptr, len) in [(65_536, 1), (0, 65_537), (u32::MAX, u32::MAX)] {
        let body = format!(
            "i32.const {} i32.const {} call $host_call",
            ptr as i32, len as i32
        );
        let err = run(&guest(16, &body)).await.unwrap_err();
        assert!(
            matches!(&err, Error::Endpoint(m) if m.contains("out of bounds") || m.contains("overflow")),
            "({ptr}, {len}): {err:?}"
        );
    }
    // The host survives and answers the next call.
    assert!(run(&guest(16, &packed(16, 3))).await.is_ok());
}

#[tokio::test]
async fn an_undecodable_host_call_is_answered_not_trapped() {
    // The guest hands back the host's own call bytes as its HostCall (they are not one), then
    // returns the host's answer as its reply: the host answered rather than trapping.
    let body = "i32.const 16 i32.const 3 call $host_call";
    let reply = run(&guest(32, body)).await.unwrap();
    assert!(!reply.is_empty(), "the host wrote an answer");
}
