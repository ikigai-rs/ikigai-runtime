//! A WASI p1 command module (R-D6 convention 3): read stdin, write its ASCII-uppercase to
//! stdout. Built for `wasm32-wasip1` and run under an empty, deterministic p1 context by
//! `ikigai-runtime-wasmtime`; exercised by `tests/wasi_p1.rs`. The transformation (not an echo)
//! proves the stdin → stdout path actually carried the bytes.

use std::io::{Read, Write};

fn main() {
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).expect("read stdin");
    let upper: Vec<u8> = input.iter().map(u8::to_ascii_uppercase).collect();
    std::io::stdout().write_all(&upper).expect("write stdout");
}
