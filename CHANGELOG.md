# Changelog

## 0.2.0

- Move to ikigai-core 0.2.0, the streams release (ledger #2261). This is a breaking
  dependency change because core types appear in the runtime public API.
- Bump all four workspace crates in lockstep and require ikigai-runtime 0.2.0 in
  the guest SDK and wasmtime implementation. The WIT package remains
  `ikigai:runtime@0.1.0`; its interface is unchanged.
