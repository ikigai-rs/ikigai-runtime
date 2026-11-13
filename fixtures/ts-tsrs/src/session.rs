//! tsrs as a J2 session module (wasm32-wasip1): the sources are the named arguments of the
//! call, each an inline file (`handler.ts`, `ikigai.d.ts`). It reaches nothing else: no
//! `host_call`, and its WASI p1 imports are linked against the empty context.
//!
//! - `urn:ts:emit`: the JavaScript, `text/javascript`.
//! - `urn:ts:check`: `ok`, or the first problem as a typed `InvalidArgument` naming the file.

use ikigai_runtime_guest::abi::{AbiArgRef, AbiModuleCall, AbiModuleError, AbiRepresentation};
use ikigai_runtime_guest::core_module::Reply;
use ikigai_runtime_guest::ikigai_module;

use crate::{CHECK, EMIT, compile};

fn dispatch(call: AbiModuleCall) -> Reply {
    let AbiModuleCall::Invoke { request, .. } = call else {
        return Reply::Error("unexpected call".into());
    };
    let mut files = Vec::new();
    for (name, arg) in &request.args {
        let AbiArgRef::Inline(bytes) = arg else {
            return Reply::ErrorTyped(AbiModuleError::InvalidArgument {
                name: name.clone(),
                detail: "a source is handed in inline".into(),
            });
        };
        match std::str::from_utf8(bytes) {
            Ok(text) => files.push((name.as_str(), text)),
            Err(_) => {
                return Reply::ErrorTyped(AbiModuleError::InvalidArgument {
                    name: name.clone(),
                    detail: "not UTF-8".into(),
                });
            }
        }
    }
    let args = match request.target.as_str() {
        "urn:ts:emit" => EMIT,
        "urn:ts:check" => CHECK,
        other => return Reply::Error(format!("no such endpoint: {other}")),
    };
    let outcome = compile(&files, args);
    if let Some(problem) = outcome.problems.first() {
        return Reply::ErrorTyped(AbiModuleError::InvalidArgument {
            name: problem.file.trim_start_matches("/src/").to_string(),
            detail: problem.to_string(),
        });
    }
    match (args == EMIT, outcome.written.into_iter().next()) {
        (true, Some((_, js))) => Reply::Resolved(AbiRepresentation::new("text/javascript", js)),
        (true, None) => Reply::Error("tsc wrote no JavaScript".into()),
        (false, _) => Reply::Resolved(AbiRepresentation::new("text/plain", "ok")),
    }
}

ikigai_module!(dispatch);
