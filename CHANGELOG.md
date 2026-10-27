# Changelog

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
