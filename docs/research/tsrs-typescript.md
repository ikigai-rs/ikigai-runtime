# tsrs as the TypeScript step: emit, check, and compile inside the sandbox (ledger #3659)

Date: 2026-11-13. Question: should [tsrs](https://github.com/oxc-project/tsrs), a Rust port of
the TypeScript 7.1 beta compiler and type checker, take over #3628's TypeScript step (esbuild
0.28.2), type-check handlers at publish time against the `ikigai` surface, and compile untrusted
TypeScript inside ikigai-runtime?

**Recommendation, in one line:** adopt tsrs for steps 2 and 3 together: type-check every handler
at publish time, and do it (and the emit) in tsrs running as a J2 session module inside the
sandbox. Do not link tsrs into the host (step 1 as asked). Retire esbuild once the publish step
runs on the sandboxed module. Until then, keep it.

## Evidence and how to repeat it

- tsrs `7.1.0-beta.tsrs1` from crates.io (published 2026-11-11), every crate pinned with `=`,
  rustc 1.99.0. Source read: [oxc-project/tsrs](https://github.com/oxc-project/tsrs) at
  `2903e16`, chiefly `crates/tsrs_wasm`, `crates/tsrs_execute/src/tsc/compile.rs`,
  `crates/tsrs_vfs` and `notes/wasm-build.md`.
- `fixtures/ts-tsrs` (its own workspace, like the other guest fixtures): one function,
  `compile`, over tsrs's in-memory `MapFS` (`tsrs_vfs::vfstest`) with tsrs's embedded
  `lib.*.d.ts`, driving tsc's own command line (`tsrs_execute::execute::command_line`) with a
  `System` that has no environment, no process spawning and no stdout. It is built twice: as a
  native driver (`ts-tsrs emit|check|time`) and as a wasm32-wasip1 J2 session module.
- `tests/js_handler.rs` (feature `js-fixtures`) builds both with cargo, so CI needs no new tool.
  `tests/js_bindings.rs` now also generates `fixtures/js-plugin/src/ikigai.d.ts`.
- Numbers: `measurements/js-handler-tsrs-linux.txt`, from
  `cargo test --release -p ikigai-runtime-wasmtime --features js-fixtures --test js_handler -- --ignored --nocapture measure`,
  on a shared Linux machine (i7-11800H). Load moved the absolute times, so compare rows within
  that file.

## 1. tsrs as the TypeScript-to-JavaScript step

**The JavaScript runs unchanged.** `tsrs_javascript_runs_unchanged_in_the_plugin` builds the
handler's module with tsrs's emit in place of esbuild's, through the same Javy plugin, and runs
`greet`, `verbs`, `vectors`, `caught` and `secret`: the same answers and refusals, typed as
before. The two outputs are the same program and differ in form only. tsc does not minify
(2,346 B against esbuild's `--minify-whitespace` 1,712 B), keeps `1_000_000` where esbuild writes
`1e6`, and writes `export function handle` where esbuild writes a trailing `export { handle }`.
After QuickJS bytecode and pre-initialization the modules differ by 1.6 KB (1,068,183 B
against 1,069,841 B), and calls cost the same (greet, warm median: 489 µs against 487 µs).

**Time.** The emit settings are esbuild's: types stripped, nothing checked or lowered
(`--noCheck --isolatedModules --target esnext --module esnext`), plus `--lib es5 --types ""
--noResolve`. The default `esnext` libraries, DOM included, made up most of the time otherwise:
19 ms warm before, 2.2 ms after, the same bytes out. `--noLib` is faster still (0.5 ms), but tsc
then reports the missing global types even under `--noCheck`.

| | first | warm (median of 20) |
|---|---:|---:|
| esbuild, a process per compile (as #3628 runs it) | 4.26 ms | 8.16 ms |
| tsrs in process, around `compile` only | 3.31 ms | 2.21 ms |
| tsrs, the whole driver process | 5.14 ms | 8.32 ms |

Per compile, the two tools cost the same; process start is most of either.

**What it adds to the build.** 24 tsrs crates and 135 packages in all. From nothing, 2m03s for
the native driver and 2m25s for the wasm module (16 threads, under load). The release driver is
16.5 MB, against esbuild's 11.4 MB binary. 3.8 MB of that is the embedded `lib.*.d.ts`
(`notes/wasm-build.md`). tsrs crates require Rust 1.99 and edition 2024, and this workspace's
floor is 1.97 (R-D17). Linking them into a published crate would raise it.

**What tsrs cannot emit.** It does not minify, and tsc does not bundle: a handler that imports
another module would need a bundler before the plugin, which takes one file. #3628 does not
bundle with esbuild either, so nothing is lost today. Everything the handler uses emits the same.

**Linked into the host: no.** tsc's driver takes a `&'static` system, so each in-process compile
leaks one (`Box::leak`, as tsrs's own wasm build does). tsrs keeps process-wide state
(thread-local arenas, interned strings, a worker pool; `notes/wasm-build.md`: "compile once, new
instance per run"). Together with the beta and the Rust 1.99 floor, that rules out linking it
into a long-running host. Step 3 runs the same code with none of these problems.

## 2. Type checking at publish time

**The declarations come from the ABI.** `generate_dts` in `tests/js_bindings.rs` derives
`fixtures/js-plugin/src/ikigai.d.ts` from the same `ABI` resource that generates `ikigai.js`
(PR 15). It declares every wire type as the codec reads and writes it, `Arg`,
`Representation`, `Codec<T>`, `Refusal` (one per `AbiModuleError` variant), `KernelError`, and
`ikigai: ikigai.Kernel` with one method per `AbiVerb` variant. The verbs are methods, not
functions, because `delete` is a reserved word. The checked-in file is pinned to the generator,
as `ikigai.js` is (`IKIGAI_WRITE_BINDINGS=1` rewrites both).

**It found two defects esbuild never could.** Checked strictly against the generated surface,
the handler as merged failed twice:

- `handler.ts:94:30: TS2304: Cannot find name 'KernelError'.` The handler named a type it
  never declared. esbuild only strips types, so it passed.
- `TS2345` on `codec.encode(codec.decode(...))`, where `codec` is a union of two codecs whose
  argument types do not meet. It ran correctly by luck of the data.

This PR fixes both in `handler.ts`: it drops its hand-written copy of the surface, names
`ikigai.KernelError`, and gives the vector loop's codec the type `ikigai.Codec<unknown>`. It now
checks clean (`the_handler_type_checks_against_the_surface_generated_from_the_abi`).

**A wrong call is refused, typed.** `a_wrong_call_is_refused_at_publish_with_its_file_line_and_message`
edits the handler three ways, and the publish check returns `Error::InvalidArgument` with
`name` the file and `detail` its line, column and tsc's message:

| edit | `detail` |
|---|---|
| a missing argument: `ikigai.source()` | `handler.ts:10:23: TS2554: Expected 1-2 arguments, but got 0.` |
| a wrong argument: `{ body: 42 }` | `handler.ts:12:33: TS2322: Type 'number' is not assignable to type 'Arg'.` |
| a verb that does not exist: `ikigai.put(...)` | `handler.ts:12:10: TS2339: Property 'put' does not exist on type 'Kernel'.` |

**Check time:** 8.6 ms for the first check in a process and 9.8 ms warm, in process. Inside the
sandbox, 33 ms per check (below).

## 3. Compiling untrusted TypeScript inside the sandbox

**tsrs's own module does not fit R-D6; one built from the same crates does.** tsrs's published
wasm build (`crates/tsrs_wasm`, shipped on npm, not as a 7.1 crate) reads files through its own
host imports, `tsrs_host.fs` and `tsrs_host.fs_take`. Its exports are `tsrs_input`/`tsrs_run`,
not the J2 session. Running it would need a new import module and a new calling convention, so
it does not fit R-D6 unchanged. `fixtures/ts-tsrs` is the same compiler behind the J2 session
instead (`src/session.rs`, about 50 lines):

- **Imports:** `wasi_snapshot_preview1` only (19 functions), linked against the empty context
  of R-D6 point 2 as amended. It does not even import `ikigai.host_call`. **Exports:**
  `ikigai_alloc`, `ikigai_free`, `ikigai_invoke`, `memory`.
- **Sources:** the call's named arguments, each an inline file (`handler.ts`, `ikigai.d.ts`),
  placed in tsrs's in-memory `MapFS`. The libraries are embedded. Nothing is read from the
  host: `tsrs_compiles_typescript_handed_to_it_inside_the_sandbox` hands it a file that
  references `/etc/hostname` and imports `/etc/passwd`, and the reply is a typed
  `InvalidArgument`: `TS6231: Could not resolve the path '/etc/hostname'`.
- **Answers:** `urn:ts:emit` returns the JavaScript (byte for byte the native driver's);
  `urn:ts:check` returns `ok`, or the first problem as a typed
  `InvalidArgument { name: file, detail: "file:line:column: TScode: message" }`.

**So it fits R-D6 unchanged**, with two costs a host has to budget for:

| | |
|---|---|
| module size | 15,182,929 B raw (no `wasm-opt`), 3,547,563 B gzip -9 |
| compile (Cranelift, no compiled cache) | 1.32 s, once per host; the compiled cache (R-D7) removes it on restart |
| emit, per call in a fresh instance | 5.3 ms median, 29.0 M fuel |
| check, per call in a fresh instance | 33.3 ms median, 178.8 M fuel |
| linear memory | a check fits in 112 MiB, not in 96 MiB. 80 MiB of that is the shadow stack, tsrs's own measured choice, kept here so that a deep expression traps rather than corrupting memory |

The memory is the one thing to settle before use. 112 MiB per compile is far above what a
handler needs (QuickJS modules run in a few MiB), so the publish step needs its own
`urn:cap:compute:memory` budget. That is a grant, not a change to R-D6.

## 4. What we would take on

- **A beta.** tsrs follows TypeScript 7.1 beta and is six weeks old; its crates appeared on
  crates.io two days ago. It takes no outside pull requests, so we cannot fix it upstream. The
  exact pin and the in-sandbox use keep a bad release contained: it can produce a wrong
  diagnostic, not reach the host.
- **Its notice.** tsrs is Apache-2.0 and derived from Microsoft's TypeScript; the embedded
  `lib.*.d.ts` files carry Microsoft's notice. A module we distribute must carry tsrs's `NOTICE`.
- **Build time** of about two minutes per target from nothing, in the `js-fixtures` CI job only.

## 5. Recommendation

- **Step 2, adopt.** A strict check against declarations generated from the ABI caught two
  real defects in a 107-line handler and refuses wrong calls with file, line and message, as the
  `InvalidArgument` a publish step returns.
- **Step 3, adopt as the way to run steps 1 and 2.** It fits R-D6 unchanged, reads only what it
  is handed, and costs 5 ms to emit and 33 ms to check per call once compiled. It needs a
  112 MiB memory grant.
- **Step 1 as asked (tsrs linked into the host), do not adopt.** It matches esbuild per compile
  but brings process-wide state, a leak per compile, a Rust 1.99 floor and a beta into the host.
  The emit should happen inside step 3's module. esbuild stays until the publish step runs
  there, and can then go: one tool emits and checks, and it is built from source rather than
  downloaded.
