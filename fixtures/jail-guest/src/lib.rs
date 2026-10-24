//! J2's five fixtures (`urn:jail:{hello,spin,balloon,sleep,boom}`) as one core-module guest on
//! the ikigai-module ABI v1 (R-D6, R-D18). Built for `wasm32-unknown-unknown` by
//! `ikigai-runtime-wasmtime`'s build script and exercised by `tests/jail.rs`.

use ikigai_runtime::session::{ModuleCall, ModuleReply};
use ikigai_runtime_guest::core_module::host_source;
use ikigai_runtime_guest::ikigai_module;

/// The contract JSON a `Describe` returns (stable, for the describe-parity test).
const DESCRIBE: &str = r#"{"endpoints":["urn:jail:hello","urn:jail:escalate","urn:jail:spin","urn:jail:balloon","urn:jail:sleep","urn:jail:boom"]}"#;

fn dispatch(call: ModuleCall) -> ModuleReply {
    let (endpoint, capability) = match call {
        ModuleCall::Invoke { endpoint, capability, .. } => (endpoint, capability),
        ModuleCall::Describe => return ModuleReply::Describe(DESCRIBE.to_string()),
        ModuleCall::HostResult(_) => {
            return ModuleReply::Error("guest received an unexpected HostResult".to_string())
        }
    };

    match endpoint.as_str() {
        // Sources `urn:host:name` through the kernel (`host_call`), under the session's
        // capability, and greets it. The book's own semantics test.
        "urn:jail:hello" => match host_source("urn:host:name", &capability) {
            Ok(name) => {
                let mut greeting = b"hello, ".to_vec();
                greeting.extend_from_slice(&name);
                ModuleReply::Resolved(greeting)
            }
            Err(e) => ModuleReply::Error(e),
        },
        // Claims FULL authority (`Capability::root()`) on its host_call, regardless of the
        // session's capability. The host clamps it to the session's, so a guest can never widen
        // its authority by being jailed — the declared-capability floor, tested across the jail.
        "urn:jail:escalate" => {
            match host_source("urn:host:name", &ikigai_runtime::Capability::root()) {
                Ok(name) => ModuleReply::Resolved(name),
                Err(e) => ModuleReply::Error(e),
            }
        }
        // Loops forever: exhausts fuel. (A counter with `black_box` so the loop is not optimized
        // away and really spends fuel each iteration.)
        "urn:jail:spin" => spin(),
        // Allocates without bound: exhausts the memory ceiling, then aborts, which the host reads
        // as OverBudget via the denied-growth flag.
        "urn:jail:balloon" => {
            let mut held: Vec<Vec<u8>> = Vec::new();
            loop {
                held.push(vec![0u8; 1 << 16]);
                core::hint::black_box(held.len());
            }
        }
        // Busy-waits past the deadline: the same spin, run by the test with generous fuel and a
        // short epoch deadline, so the epoch (not fuel) ends it.
        "urn:jail:sleep" => spin(),
        // Panics (abort on wasm): a contained trap, which the host reads as Endpoint.
        "urn:jail:boom" => panic!("boom"),
        other => ModuleReply::Error(format!("no such endpoint: {other}")),
    }
}

fn spin() -> ModuleReply {
    let mut i: u64 = 0;
    loop {
        i = i.wrapping_add(1);
        core::hint::black_box(i);
    }
}

ikigai_module!(dispatch);
