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
     `host_call` import): ikigai-module's own postcard session, byte for byte, which a guest
     builds from `ikigai-module-abi`'s wire-only types. `JailTransport` mounts such a module
     through ikigai-module's `ModuleSpace`, so one module is servable in-process, over a socket
     and in the jail;
  3. **WASI p1 stdin → stdout** for existing command modules.
- **The component tier** runs WebAssembly components on wasmtime, in two worlds of the
  `ikigai:runtime` WIT: `endpoint` (no WASI authority at all, whatever the caller holds) and
  `app` (the WASI a grant unlocks, built per call). Each exported function becomes an endpoint
  under `urn:runtime:app:{app}:{interface}:{func}` with ArgSpecs from its WIT types; each import
  is bound to a kernel request at run time (`wasi:http` to `urn:http:*`), and an unbound import
  refuses the mount.

Every request a guest makes of the kernel (a `host_call`, a component's `kernel` import, a bound
import, `wasi:http`) runs through the invocation running the guest, under the session's
capability: traced, golden-threaded and denied like any other request. A guest never gets
authority by being jailed, and fuel, epoch and memory budgets refuse as `OverBudget`.

## The workspace

| crate | what it holds |
|---|---|
| **`ikigai-runtime`** | engine-neutral, **wasm-clean** core: `Limits` (the budget meet, and the budget a caller's `urn:cap:compute:*` grants state), `Grants` and the grant → WASI plan (a pure, table-tested function), the `CoreEngine` trait and the async `HostResolver` seam (with `KernelResolver` over a whole kernel), the ABI v1 packing and names with the session's wire types re-exported from `ikigai-module-abi`, the projection types, and the content-addressed loader (`load_artifact` by digest). |
| **`ikigai-runtime-wasmtime`** | both tiers on wasmtime: the core-module tier (three conventions, `JailTransport`), the component tier (`ComponentEngine`, `ComponentSpace`, the two worlds, projection, import binding, `wasi:http` through the kernel), a fresh `Store` per call, fuel · epoch · memory from `Limits` (or an epoch-only engine), and the R-D10 error mapping. |
| **`ikigai-runtime-wit`** | the `ikigai:runtime@0.1.0` WIT package (the `kernel` import, the `endpoint-api` export, the `endpoint` and `app` worlds), with tests that it parses, holds those interfaces, and that the copies the host and guest bindings are generated from are identical to it. |
| **`ikigai-runtime-guest`** | the Rust guest SDK: the core-module (J2) guest ABI on `ikigai-module-abi`, and wit-bindgen bindings for the `endpoint` world (synchronous p2). |
| **`fixtures/`** | guest crates built in CI: the J2 guest, a WASI p1 command, an `endpoint`-world component, a stock component projected export by export, and R1's probe as a component. wasm32 only — excluded from the native workspace, built by `ikigai-runtime-wasmtime`'s build.rs behind the `jail-fixtures` feature. |

## Status

R3a: the scaffold and the core-module tier. R3b: the component tier and the probe table. Bundles,
signing and the shell (R3c), and the first apps (R4, R5) follow. Nothing here runs anyone else's
code yet; the clamp, signing and a red-team round come first (R-D20).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
