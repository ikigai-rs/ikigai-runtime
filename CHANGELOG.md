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

## 0.2.0

- Move to ikigai-core 0.2.0, the streams release (ledger #2261). This is a breaking
  dependency change because core types appear in the runtime public API.
- Bump all four workspace crates in lockstep and require ikigai-runtime 0.2.0 in
  the guest SDK and wasmtime implementation. The WIT package remains
  `ikigai:runtime@0.1.0`; its interface is unchanged.
