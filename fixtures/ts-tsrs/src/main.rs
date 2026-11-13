//! The native driver `tests/js_handler.rs` runs (ledger #3659). tsrs is linked into this
//! process; every time printed is measured inside it, around `ts_tsrs::compile` alone.
//!
//! - `ts-tsrs emit <in.ts> <out.js>`: strip types, as esbuild does.
//! - `ts-tsrs check <surface.d.ts> <handler.ts>`: type-check the handler against the surface;
//!   one `problem` line per diagnostic, `file:line:column: TScode: message`.
//! - `ts-tsrs time <surface.d.ts> <handler.ts> <rounds>`: the first emit and check in this
//!   process (cold) and the median of `rounds` more (warm).
//!
//! Exit status 0 when tsc reported nothing, 1 otherwise.

use std::path::Path;
use std::time::Duration;

use ts_tsrs::{CHECK, EMIT, Outcome, compile};

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn report(outcome: &Outcome) -> i32 {
    for problem in &outcome.problems {
        println!("problem {problem}");
    }
    println!("elapsed {:?}", outcome.elapsed);
    i32::from(!outcome.problems.is_empty() || outcome.status != 0)
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort();
    samples[samples.len() / 2]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let status = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["emit", input, output] => {
            let source = read(input);
            let outcome = compile(&[(&name(input), &source)], EMIT);
            if let Some((_, js)) = outcome.written.first() {
                std::fs::write(output, js).unwrap();
            }
            if outcome.written.is_empty() {
                1
            } else {
                report(&outcome)
            }
        }
        ["check", surface, handler] => {
            let (surface_text, handler_text) = (read(surface), read(handler));
            let outcome = compile(
                &[
                    (&name(surface), &surface_text),
                    (&name(handler), &handler_text),
                ],
                CHECK,
            );
            report(&outcome)
        }
        ["time", surface, handler, rounds] => {
            let (surface_text, handler_text) = (read(surface), read(handler));
            let rounds: usize = rounds.parse().unwrap();
            let files = [
                (name(surface), surface_text),
                (name(handler), handler_text.clone()),
            ];
            let files: Vec<(&str, &str)> = files
                .iter()
                .map(|(n, t)| (n.as_str(), t.as_str()))
                .collect();
            let handler_only = [(name(handler), handler_text)];
            let handler_only: Vec<(&str, &str)> = handler_only
                .iter()
                .map(|(n, t)| (n.as_str(), t.as_str()))
                .collect();
            for (label, files, args) in [("emit", &handler_only, EMIT), ("check", &files, CHECK)] {
                let cold = compile(files, args).elapsed;
                let warm = median((0..rounds).map(|_| compile(files, args).elapsed).collect());
                println!("{label} cold {cold:?} warm {warm:?}");
            }
            0
        }
        _ => {
            eprintln!(
                "usage: ts-tsrs emit <in.ts> <out.js> | check <surface.d.ts> <handler.ts> | time <surface.d.ts> <handler.ts> <rounds>"
            );
            2
        }
    };
    std::process::exit(status);
}
