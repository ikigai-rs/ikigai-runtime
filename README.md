# ikigai-runtime

**A reusable, capability-gated WebAssembly host for ikigai.** A WebAssembly artifact is a
resource, loaded by digest and run with **no authority at all**; every authority it gets is an
ikigai capability mapped onto the engine's own mechanisms or served by the kernel itself. Its
exports become ikigai endpoints, its imports are bound to kernel requests, and its budgets come
from grants. The same app runs in the jail, natively under the capability clamp, and in a page
where a page can run it.

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

## Pooling allocator decision (R-D7)

The runtime keeps pooling **off** and reuses `InstancePre` with Wasmtime's default on-demand
allocator. The measurement harness compares re-linking, cached `InstancePre`, and cached
`InstancePre` with Wasmtime 50's default pool. Production configuration is unchanged.

The original memory-free harness reproduced the speedup on an Apple M5 Pro (macOS arm64,
Rust 1.99.1, release): 33.127 microseconds per call re-linking, 4.803 with `InstancePre`, and
1.853 with pooling (5,000 calls). That alone does not establish the memory cost or the benefit
for a Rust std guest or a guest with a large dirty memory.

The extended comparison runs each guest/strategy in a fresh process, warms up, then reports
five batches and their median, minimum, and maximum per-call times. Each call creates and
drops a Store and WASI context, instantiates, finds and calls `add`, and checks the sum.
Wasmtime 50 handles canonical post-return internally. Compilation and cached linking are
outside timing; re-linking is inside its timed calls. The Rust std guest is `fixtures/comp-calc` with its unused lookup import
bound to a refusing stub. The large WAT guest starts with 256 MiB and writes every 4 KiB each
call, so the measurement includes dirty-page reset rather than just lazy reservation. It uses
2 warmups and 5 batches of 20 calls; the other guests use 100 warmups and 5 batches of 1,000.
These are serial allocator microbenchmarks without production fuel/epoch metering or kernel
routing, not throughput or end-to-end latency predictions.

Reproduce on Linux or macOS with `ps` available (`procps` on Linux):

```sh
rustup target add wasm32-unknown-unknown wasm32-wasip1 wasm32-wasip2
cargo test -p ikigai-runtime-wasmtime --release --features jail-fixtures \
  --test measure measure_instance_pre_and_pooling -- --ignored --nocapture
```

Linux evidence was collected by a temporary release driver in the existing `jail-fixtures`
CI job at commit `55a12f7`; macOS was measured locally with the same timed path. The driver
is removed after collection; the final harness remains ignored in ordinary CI. Only parent
process host-metadata logging was added after that Linux run. `memory` rows report process virtual size (VSZ) and resident
set (RSS) in KiB at baseline, engine creation, compilation, warm idle, after the timed calls, with a live called
guest, and after its Store drops. Engine-minus-baseline isolates the pool's up-front mapping
cost; live-minus-idle helps distinguish resident guest pages from address space reserved but
not resident. RSS is a process snapshot, not a physical-memory commitment or peak guarantee.

Results recorded on 2026-11-10 (UTC), Wasmtime 50.0.0, release. Times are the median of five
batch averages in microseconds per call; the raw output includes the batch range.

| Host | Guest | Re-link | InstancePre | InstancePre + pool |
|---|---|---:|---:|---:|
| Linux x86_64, GitHub Ubuntu 26.04.1, Rust 1.99.1 | Trivial WAT, no memory | 137.172 | 21.862 | 4.897 |
| Linux x86_64, GitHub Ubuntu 26.04.1, Rust 1.99.1 | Rust std `comp-calc` | 236.623 | 90.970 | 32.991 |
| Linux x86_64, GitHub Ubuntu 26.04.1, Rust 1.99.1 | WAT, 256 MiB dirtied per call | 127,523.995 | 125,964.648 | 128,560.526 |
| macOS arm64, Apple M5 Pro, Rust 1.99.1 | Trivial WAT, no memory | 34.207 | 5.574 | 2.031 |
| macOS arm64, Apple M5 Pro, Rust 1.99.1 | Rust std `comp-calc` | 47.245 | 15.792 | 15.284 |
| macOS arm64, Apple M5 Pro, Rust 1.99.1 | WAT, 256 MiB dirtied per call | 10,380.379 | 10,215.677 | 10,345.446 |

The tested pool is `PoolingAllocationConfig::default()` on a 64-bit host: 1,000 slots each
for components, core instances, memories, tables, async stacks, and GC heaps; at most one
memory and one table per core module; at most 4 GiB per memory, 20,000 elements per table,
and 1 MiB each for core/component instance metadata. Async stacks are 2 MiB each and the
unused warm-slot limit is 100. These are allocator ceilings, not capability grants or a
promise that 1,000 arbitrary components fit concurrently: a component can consume multiple
core instances/memories/tables. The test changes none of these defaults, does not enable
memory protection keys or PAGEMAP_SCAN, and does not raise the default resident-page retention.

| Host | Allocator | Engine VSZ increase (GiB) | Engine RSS increase (MiB) |
|---|---|---:|---:|
| Linux | On demand (InstancePre) | 0.000 | 2.21–2.44 |
| Linux | Pool | 4,033.391 | 8.25–8.42 |
| macOS | On demand (InstancePre) | 0.002 | 0.83–0.84 |
| macOS | Pool | 4,033.404 | 1.36–1.38 |

Engine increases are relative to each child process's baseline, before compilation. The
macOS pool reserves about 3.94 TiB of address space, mostly inaccessible/not resident. It
adds about 0.5 MiB more immediate RSS than on-demand allocation, not terabytes of RAM.
For the dirty-memory guest, the pool process RSS is 13.20 MiB after timed calls, 269.20 MiB
with a live called guest, and 13.20 MiB after dropping that Store. The on-demand InstancePre
process is 12.78 / 268.80 / 12.78 MiB at those stages. On Linux, those stages are 23.21 / 279.21 / 23.21 MiB for the pool and
22.60 / 278.61 / 22.60 MiB for on-demand InstancePre. The 256 MiB is resident while used and
released at Store drop in these runs. On-demand allocation also adds about 4.06 GiB of VSZ
while this guest is live; the pool uses its existing reservation. Raw snapshots also cover the trivial and std guests.

**Recommendation: leave pooling off.** Cached linking earns its place on both hosts. Pooling
lowers the trivial guest's time by 78% on Linux and 64% on macOS. The Linux std result is a
meaningful 64% reduction (about 58 microseconds saved per call), while the macOS std reduction
is about 3%. The dirty-memory guest is about 2% slower with pooling on Linux and about 1%
slower on macOS (the macOS batch ranges overlap). There is no measured dirty-memory benefit.
The Linux std result warrants investigating a smaller bounded pool for a host with that
workload, but these serial measurements do not establish its required capacity or behavior
under load. Keep the general runtime on demand: this evidence does not justify a global pool
reservation and fixed admission ceilings for unspecified guest sizes and concurrency. A future opt-in host configuration would need explicit slot,
memory, table, and metadata limits and typed `OverBudget` refusals for guests or concurrent
calls that cannot fit; raw Wasmtime allocation errors would not suffice. This change adds no
pool to production, so it adds no new refusal path and changes no default or budget behavior.

The Linux VM's CPU model was not captured; its image version is recorded with the raw output.
Compare strategies within each host, not absolute speed across these two different machines.
Future runs print CPU and toolchain metadata before launching the isolated cases.
These measurements do not cover concurrency, steady-state reuse of live instances, tuning
resident retention, a smaller bounded pool, or other operating systems/CPUs. They do not prove
pooling is never useful. Full macOS evidence: [original harness](measurements/pooling-original-macos.txt)
and [extended comparison with host metadata](measurements/pooling-macos.txt). Linux evidence:
[extracted raw measurements](measurements/pooling-linux.txt) and the
[green fixture CI job](https://github.com/ikigai-rs/ikigai-runtime/actions/runs/42739206317/job/129575811880).

## Accessibility

What these crates emit that people read (run failures, refusals and argument errors), the rules
that text follows and how to report a barrier are in [ACCESSIBILITY.md](ACCESSIBILITY.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
