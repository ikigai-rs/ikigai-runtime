# ikigai-runtime

**A reusable, capability-gated WebAssembly host for ikigai.** A WebAssembly artifact is a
resource, loaded by digest and run with **no authority at all**; every authority it gets is an
ikigai capability mapped onto the engine's own mechanisms or served by the kernel itself. Its
exports become ikigai endpoints, its imports are bound to kernel requests, and its budgets come
from grants. The same app runs in the jail, natively under the capability clamp, and in a page
where a page can run it.

The design record is `claude/runtime/DECISIONS.md` in `ikigai-devtools` (ledger
[#2165](http://localhost:1062/l/default/item/2165)); this repository is R3, built to it.

## The two tiers

- **The core-module tier** runs plain core modules through the `CoreEngine` trait (wasmtime
  first, wasmi next). Three calling conventions, chosen per module from its exports:
  1. **numeric signatures** directly (`i32`/`i64`/`f32`/`f64` in, numeric out);
  2. the **ikigai-module ABI v1** (`ikigai_alloc` / `ikigai_free` / `ikigai_invoke` plus a
     `host_call` import, a postcard session) — "J2 absorbed";
  3. **WASI p1 stdin → stdout** for existing command modules.
- **The component tier** (R3b) runs WebAssembly components on wasmtime, with a WIT → ikigai
  projection: each export becomes a derived resource under `urn:runtime:app:{app}:…`.

## The workspace

| crate | what it holds |
|---|---|
| **`ikigai-runtime`** | engine-neutral, **wasm-clean** core: `Limits` (the budget meet), `Grants` and the grant → WASI plan (a pure, table-tested function), the `CoreEngine` trait, the J2 session types, the projection types, the loader/compiled-cache types, and the `Space` adapter. |
| **`ikigai-runtime-wasmtime`** | the wasmtime implementation of the core-module tier: the three conventions, a fresh `Store` per session, fuel · epoch · memory from `Limits`, `host_call` clamped to the session's capability, and the R-D10 error mapping (fuel/budget → `OverBudget`, never transient). |
| **`ikigai-runtime-wit`** | the `ikigai:runtime@0.1.0` WIT package (the `kernel` import, the `endpoint` export, the `endpoint` and `app` worlds), with a test that it parses and holds those interfaces. |
| **`ikigai-runtime-guest`** | the Rust guest SDK. R3: the core-module (J2) guest ABI; the wit-bindgen component SDK is R3b. |
| **`fixtures/`** | guest crates built in CI, one per calling convention, plus the probe stub. wasm32 only — excluded from the native workspace, built by `ikigai-runtime-wasmtime`'s build.rs behind the `jail-fixtures` feature. |

## Status

R3a: the scaffold and the core-module tier. The component tier (R3b), bundles and signing
(R3c), and the first apps (R4, R5) follow. Nothing here runs anyone else's code yet; the clamp,
signing and a red-team round come first (R-D20).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
