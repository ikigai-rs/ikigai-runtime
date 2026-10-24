//! The WASI p1 command-module convention (R-D6 convention 3), end to end through the `p1-cmd`
//! fixture. This also verifies the "Still to measure" item: wasmtime-wasi 50's p1 path serves a
//! stdin → stdout command module. Behind the `jail-fixtures` feature.
#![cfg(feature = "jail-fixtures")]

use std::sync::Arc;

use ikigai_runtime::space::Convention;
use ikigai_runtime::{Call, Capability, CoreEngine, Error, HostResolver, Limits, Output};
use ikigai_runtime_wasmtime::{detect_convention, WasmtimeCoreEngine};

const P1CMD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/p1cmd.wasm"));

struct NoHost;
impl HostResolver for NoHost {
    fn source(&self, _iri: &str, _cap: &Capability) -> Result<Vec<u8>, Error> {
        Err(Error::NotFound(
            "a command module makes no host calls".into(),
        ))
    }
}

#[test]
fn a_p1_command_module_transforms_stdin_to_stdout() {
    let engine = WasmtimeCoreEngine::new(Limits::fuel(u64::MAX)).unwrap();
    let compiled = engine.compile(P1CMD).unwrap();
    assert_eq!(detect_convention(&compiled), Convention::Command);

    let out = engine
        .invoke(
            &compiled,
            Call::Command {
                stdin: b"hello, camel",
            },
            &Limits::fuel(u64::MAX),
            Arc::new(NoHost),
        )
        .unwrap();
    match out.output {
        Output::Command { stdout } => assert_eq!(stdout, b"HELLO, CAMEL"),
        _ => panic!("expected a command output"),
    }
}
