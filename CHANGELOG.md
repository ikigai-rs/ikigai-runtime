# Changelog

## Unreleased

- **0.5.0: grants place exclusions the way ikigai-fs and ikigai-http do** (a minor release: new
  public fields). A run's preopens and sockets now honor every exclusion its grants hold, read
  as the rest of the ecosystem reads it.
  - Sockets: a raw socket connect or send is answered by `ikigai_http::net_allows_port` under
    the run's IP-literal `urn:cap:net:` grants and every `urn:cap:net:` exclusion, so
    `net:10.0.0.5` with `net:-10.0.0.5:22` reaches every port but 22, an exclusion that ties a
    grant wins, and a malformed exclusion refuses every address. The plan carries this list as
    `NetAccess::socket_scopes`. A plan with an IP-literal grant and an exclusion of a path that
    could fall on an IP is refused with a sentence, since a socket carries no path.
  - Preopens: `WasiPlan::fs_denies` (new, with the `FsDeny` type) carries the `fs` exclusions,
    and `apply_plan` refuses a preopen one of them falls in, placed as ikigai-fs places it:
    against the jail root as mounted and as the filesystem spells it, through links (a deny on
    `current/secret` binds `releases/v2/secret`), by on-disk name, and without case where the
    path does not exist yet. The placement is a copy of ikigai-fs 0.6.0's, which is private
    there; it belongs in ikigai-fs as one public function.
  - A preopen is always the granted directory, even when a component of it is missing, and is
    opened one component at a time from a handle on the jail root without following a link
    (`openat` with `O_NOFOLLOW`). On Linux wasmtime is handed that opened directory; on other
    Unix hosts it is handed the path the walk checked.
  - Upgrading: no persisted state. A guest that relied on an excluded path or port being
    reachable is now refused, with a sentence naming the exclusion. `ikigai-runtime-guest` and
    `ikigai-runtime-wit` move to 0.5.0 with the workspace and do not change.

- **0.4.1: a mounted jail meets the caller's compute grant** (ledger #4435, from the audit in
  #4034; a patch: no public API changes). A J2 module mounted through `JailTransport` ran on the
  mount's limits alone, so a caller granted 1 unit of fuel could spend the mount's whole budget.
  Each `invoke` now runs on the meet of the mount's limits and the caller's
  `urn:cap:compute:*` grants (the smaller of each), as the component tier already does. The
  mount's limits are a ceiling: a caller granted more than the mount still stops at the mount's
  budget. Running out is the same typed `OverBudget` refusal as before. Mount-time `Manifest` and
  `Cards` calls have no caller and still run on the mount's limits.

- **0.4.1: bound-import values round-trip** (ledger #4033, a patch: no public API changes). A
  component calling a bound import, and the component it reaches, now agree on every value the
  type map names. An empty `list<u8>` argument is sent as no bytes, not the text `[]`. An
  `option`'s `none` argument is left out (absent, as the exporting side reads it), not sent as
  the text `null`. A `result` answered with an `err` (served as `application/problem+json`)
  arrives as that `err`, not as an `ok` holding the problem's JSON. A NotFound answer to an
  `option` result is `none` instead of a trap. A float that JSON cannot hold (NaN, infinity),
  written as its text, is read back from that text. New hermetic tests in `tests/wat_session.rs`
  check that a J2 guest handing the host out-of-bounds or overflowing pointers and lengths is
  refused, never a panic.

- **ikigai-core 0.4.0** (the core 0.4.0 cascade, epic #3893, ledger #3923): the workspace now pins
  ikigai-core 0.4.0 (scope opacity, structured denials), ikigai-module 0.7.0 and ikigai-http
  0.8.0, and moves to 0.4.0, a minor bump because core types are part of the public API.
  `Error::Denied` carries core's `Denial`. The `ikigai:runtime` WIT `denied` case is still a
  string: it carries the denial's message, and a denial coming back from a guest is built from
  that message. No other behavior changes.

- **Research: tsrs as the TypeScript step** (ledger #3659), behind the test-only `js-fixtures`
  feature; no public API changes. `fixtures/ts-tsrs` compiles TypeScript with tsrs
  7.1.0-beta.tsrs1 (pinned exactly) over an in-memory file system, natively and as a J2
  session module. `tests/js_bindings.rs` also generates `fixtures/js-plugin/src/ikigai.d.ts`
  from the ABI. `tests/js_handler.rs` runs tsrs's JavaScript in the plugin, type-checks the
  handler against the generated declarations (a wrong call is a typed `InvalidArgument` with
  its file, line and message), and compiles TypeScript inside the sandbox. The handler now
  checks clean: it named an undeclared `KernelError` and called a union of codecs. esbuild
  stays. Report: `docs/research/tsrs-typescript.md`; numbers:
  `measurements/js-handler-tsrs-linux.txt`.
- **Generated JavaScript guest bindings for the J2 session** (ledger #2414, stage 1), behind the
  test-only `js-fixtures` feature; no public API changes. `tests/js_bindings.rs` states
  ikigai-module-abi 0.1.0's wire types as data and generates from them
  `fixtures/js-plugin/src/ikigai.js`: a postcard codec for every type and the `ikigai` surface,
  one function per verb (`source`, `sink`, `exists`, `delete`, `meta`), each taking a target and
  named arguments and returning the representation. The `js-plugin` fixture no longer
  hand-writes the verbs; it gives the script the raw `host_call` and evaluates it at
  pre-initialization. The checked-in file must equal the generator's output, the vendored ABI
  vectors must equal what the linked Rust crate encodes, and the generated codec must
  re-encode every vector byte for byte inside QuickJS.
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
