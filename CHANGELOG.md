# Changelog

## Unreleased

- **Spike: a TypeScript handler as a JavaScript engine module** (ledger #3628), behind the
  test-only `js-fixtures` feature; no public API changes. `fixtures/js-plugin` is a Javy plugin
  (javy-plugin-api 7.1.0, QuickJS, wasm32-wasip1) that exports the J2 allocator and
  `ikigai_invoke` and gives JavaScript `ikigai.source` and `ikigai.sink` over `ikigai.host_call`;
  a typed refusal is a JavaScript exception named for its class, and keeps its type if it
  escapes. `tests/js_handler.rs` compiles `fixtures/js-handler/handler.ts` with esbuild 0.28.2,
  pre-initializes the module with javy 9.1.0 (`init-plugin --deterministic`), caches each
  derived form under the digest of its input, and checks that the handler reads and writes what
  its grant allows and is refused, typed, by the grant for what it does not. Measurements are in
  `measurements/js-handler-macos.txt`.
  CI does not run it yet: the job that installs both tools with their digests checked is
  proposed in the pull request, since adding a workflow needs a maintainer.
- **A J2 session module may also import WASI p1** (R-D6 point 2, amended 2026-11-12, ledger
  #3628). A module exporting `ikigai_alloc`/`ikigai_free`/`ikigai_invoke` and importing
  `ikigai.host_call` may import `wasi_snapshot_preview1` too, as a JavaScript engine compiled to
  wasm does. Those imports are linked against the command convention's empty, deterministic
  context: no preopens, env, args or sockets, a fixed clock, and a seeded RNG that is not a source
  of secrets. stdin reads EOF. stdout and stderr go to a capture bounded at
  `SESSION_STDIO_CAPACITY` (64 KiB each); past it writes are dropped and counted. A host reads
  the capture through `WasmtimeCoreEngine::with_session_stdio`; otherwise it is discarded. It
  never reaches the host's stdio or the reply. `proc_exit`, with any code, ends the call with an
  endpoint error naming the code. `host_call` stays the only route out. A module without p1
  imports runs exactly as before. Tested by `tests/session_wasi.rs` (WAT, always on).
- **Compiled forms on disk** (R-D7, ledger #3339): `WasmtimeCoreEngine::with_compiled_cache` and
  `ComponentEngine::with_compiled_cache` take a `CompiledCache`, the host's own directory. After
  compiling, the engine writes the `.cwasm` to `{dir}/{engine}/{digest}.cwasm`; a restarted host
  loads it instead of compiling. An entry is deserialized only if its header names the identical
  artifact digest and compatibility hash and its body matches the blake3 hash it records; a
  corrupt or mismatched entry is compiled again, overwritten, and reported in the
  `CacheOutcome` that `compile_traced` returns. No API loads a `.cwasm` from anywhere else.
  Measured in `measurements/restart-macos.txt`: the first answer after a restart of a Rust std
  component drops from 9.8 ms to 1.3 ms (median of 7, release, Apple silicon).

## 0.3.0

- Move to ikigai-core 0.3.0, the scoped content store (ledger #2965): held content is scoped to
  whoever delivered it, a principal-less hold is root's, and `ContentStore::put`/`put_replay` are
  root-only. This is a breaking dependency change because core types appear in the runtime's
  public API. No crate here calls `put` or `put_replay`, so no call site moves.
- Move to ikigai-module 0.6.0 and ikigai-http 0.6.0, their releases on ikigai-core 0.3.0.
  ikigai-module-abi stays at 0.1.0: it links no ikigai-core.
- Bump all four workspace crates in lockstep and require ikigai-runtime 0.3.0 in the guest SDK and
  wasmtime implementation. The WIT package remains `ikigai:runtime@0.1.0`; its interface is
  unchanged.

## Unreleased (R3b)

- **The J2 session is ikigai-module's** (R-D6 as amended, option (b)): guests build it from
  `ikigai-module-abi` 0.1.0's wire-only types, hosts keep ikigai-module 0.5.1's typed session and
  convert at the boundary. R3a's own `ModuleCall`/`ModuleReply` subset is gone (breaking for a
  guest or host that used it). `session` keeps the `u64` packing and gains the ABI's export and
  import names.
- `HostResolver` is async and takes a typed `Request` (all five verbs), answering a
  `Representation`; `KernelResolver` issues through `Kernel::issue`. `CoreEngine::invoke` is
  async. (Breaking.)
- `JailTransport`: a jailed ABI v1 module mounted through ikigai-module's `ModuleSpace`.
- The guest SDK replies with `core_module::Reply`, the guest's role of `AbiModuleReply`, encoded
  at the same indexes.
- **The component tier**: `ComponentEngine`, `ComponentSpace`, the `endpoint` and `app` worlds,
  export projection (R-D4), import binding by configuration (R-D5), `wasi:http` through
  `urn:http:*`, budgets, and the component SDK in `ikigai-runtime-guest`.
- `Metering::EpochOnly`: an engine without fuel, which refuses a fuel budget rather than ignoring it.
- `Limits::from_grants`, `load_artifact`, `artifact_iri`.
- R1's probe table as tests.
- `wasi_plan` returns `Result<WasiPlan, PlanError>` (breaking): an `fs` grant whose path is not
  absolute or has an empty, `.` or `..` segment is refused, naming the grant, never normalized
  (`grants::check_fs_path`). Under a root ceiling such a grant used to preopen a directory
  outside the jail root. `apply_plan` checks again.
- **An `fs` grant names a HOST path, exactly as in ikigai-fs** (breaking): one grant string, one
  directory, whether the guest reaches it through WASI or through `urn:file:*`.
  `urn:cap:fs:read:/srv/jail/data` with the jail root `/srv/jail` preopens `/srv/jail/data` and
  the guest sees `/data` (`grants::guest_path_in_jail`). A grant that is not the jail root or
  under it, `/` included, is refused naming the grant and the jail root; it used to be served
  from `<jail root>/<path>`. `FsPreopen::guest_path` is now `host_path`.
- A jail root of `/` (or one that resolves to it) is refused when the component is mounted
  (`wasi::check_jail_root_path`, `grants::check_jail_root`).
- An `fs` exclusion at or under a preopened path refuses the plan, naming both scopes
  (`urn:cap:fs:read:/ws` + `urn:cap:fs:read:-/ws/secret`): a WASI preopen cannot exclude a path
  inside it, and the plan used to drop every exclusion and preopen all of `/ws`. An exclusion
  under no preopen changes nothing.
- **A preopen needs `fs:read`** (breaking): a WASI preopen always reads (wasmtime's `ReadWrite`
  includes read), so a path with no `read` grant gets no preopen at all and its writes go through
  the kernel. `read` alone is `ReadOnly`; `read` + `write` + `delete` is `ReadWrite`. `write` +
  `delete` alone used to be a `ReadWrite` preopen, and the guest could read through it.
- **No symbolic link between the jail root and a preopen**: each component below the root, down
  to the preopen directory, is checked with `symlink_metadata`, and a link anywhere refuses the
  grant, naming it and the link, as ikigai-fs refuses a link anywhere in a path. A grant naming
  `<jail>/data`, where `data` linked out of the jail, used to preopen the link's target. The root
  itself may still be a link (its canonical spelling is placed too). A link inside a preopen is
  cap-std's to refuse, and a test pins that it does.

## 0.2.0

- Move to ikigai-core 0.2.0, the streams release (ledger #2261). This is a breaking
  dependency change because core types appear in the runtime public API.
- Bump all four workspace crates in lockstep and require ikigai-runtime 0.2.0 in
  the guest SDK and wasmtime implementation. The WIT package remains
  `ikigai:runtime@0.1.0`; its interface is unchanged.
