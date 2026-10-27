//! J2's five fixtures (`urn:jail:{hello,spin,balloon,sleep,boom}`), plus `escalate`, as one
//! core-module guest on the ikigai-module ABI v1 (R-D6, R-D18), built from the wire-only session
//! types (`ikigai-module-abi`, R-D6 as amended). Built for `wasm32-unknown-unknown` by
//! `ikigai-runtime-wasmtime`'s build script and exercised by `tests/jail.rs`.

use ikigai_runtime_guest::abi::{
    AbiCapability, AbiModuleCall, AbiModuleCard, AbiModuleManifest, AbiModuleRewrite,
    AbiRepresentation, AbiSpaceEntry,
};
use ikigai_runtime_guest::core_module::{host_source, Reply};
use ikigai_runtime_guest::ikigai_module;

/// The endpoints this guest binds, each with its card (core's `Description` as JSON text: the
/// guest holds the text and links no JSON code).
const ENDPOINTS: &[(&str, &str, &str)] = &[
    ("urn:jail:hello", "hello", "Greet the host's name, read through the kernel."),
    ("urn:jail:escalate", "escalate", "Claim root on a host call; the host clamps it."),
    ("urn:jail:spin", "spin", "Loop forever: exhausts fuel."),
    ("urn:jail:balloon", "balloon", "Allocate without bound: exhausts memory."),
    ("urn:jail:sleep", "sleep", "Busy-wait past a deadline."),
    ("urn:jail:boom", "boom", "Panic: a contained trap."),
];

fn entries() -> Vec<AbiSpaceEntry> {
    ENDPOINTS
        .iter()
        .map(|(pattern, id, _)| AbiSpaceEntry {
            pattern: (*pattern).to_string(),
            endpoint: (*id).to_string(),
            origin: None,
        })
        .collect()
}

fn cards() -> Vec<AbiModuleCard> {
    ENDPOINTS
        .iter()
        .map(|(pattern, id, summary)| AbiModuleCard {
            entry: AbiSpaceEntry {
                pattern: (*pattern).to_string(),
                endpoint: (*id).to_string(),
                origin: None,
            },
            description: format!(
                r#"{{"id":"{id}","title":"{id}","summary":"{summary}","verbs":["Source"],"inputs":[],"outputs":["text/plain"]}}"#
            ),
        })
        .collect()
}

fn dispatch(call: AbiModuleCall) -> Reply {
    let (target, capability) = match call {
        AbiModuleCall::Invoke {
            request,
            capability,
        } => (request.target, capability),
        AbiModuleCall::Describe => return Reply::Bindings(Some(entries())),
        AbiModuleCall::Manifest => {
            return Reply::Manifest(AbiModuleManifest {
                entries: Some(entries()),
                rewrite: AbiModuleRewrite::None,
            })
        }
        AbiModuleCall::Cards => return Reply::Cards(cards()),
        _ => return Reply::Error("unexpected call".to_string()),
    };

    match target.as_str() {
        // Sources `urn:host:name` through the kernel (`host_call`), under the session's
        // capability, and greets it. A refusal keeps its type across the jail.
        "urn:jail:hello" => match host_source("urn:host:name", &capability) {
            Ok(name) => {
                let mut greeting = b"hello, ".to_vec();
                greeting.extend_from_slice(&name.bytes);
                Reply::Resolved(AbiRepresentation::new("text/plain", greeting))
            }
            Err(e) => Reply::ErrorTyped(e),
        },
        // Claims FULL authority (`root`) on its host_call, regardless of the session's capability.
        // The host clamps it to the session's, so a guest can never widen its authority by being
        // jailed — the declared-capability floor, tested across the jail.
        "urn:jail:escalate" => match host_source("urn:host:name", &AbiCapability::root()) {
            Ok(name) => Reply::Resolved(name),
            Err(e) => Reply::ErrorTyped(e),
        },
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
        other => Reply::Error(format!("no such endpoint: {other}")),
    }
}

fn spin() -> Reply {
    let mut i: u64 = 0;
    loop {
        i = i.wrapping_add(1);
        core::hint::black_box(i);
    }
}

ikigai_module!(dispatch);
