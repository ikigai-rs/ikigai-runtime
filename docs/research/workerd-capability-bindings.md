# workerd's capability bindings beside ikigai-runtime (spike, ledger #3629)

Date: 2026-11-12. Question: should TypeScript we host run in workerd, Cloudflare's open-source
Workers runtime, rather than (or beside) the path ledger #3628 measured, a JavaScript engine
compiled to WebAssembly (QuickJS under Javy) inside ikigai-runtime under a grant?

**Recommendation, in one line:** keep #3628's path as the way to run TypeScript we host; do not
take workerd on now. workerd buys a fast JIT and a familiar API at the price of a second
authority model, a second process, a sandbox its own authors say is not hardened, and a
C++/Bazel dependency that releases daily. Revisit only for a job that is CPU-bound JavaScript
from code we trust, and then run workerd beside ikigai, never as the thing that holds grants.

## Evidence and how to repeat it

- workerd `v1.20261113.1` (reports `workerd 2026-11-13`), the prebuilt
  `@cloudflare/workerd-darwin-arm64` binary from npm, run on macOS arm64.
- Source read: the `v1.20261113.1` tag of
  [cloudflare/workerd](https://github.com/cloudflare/workerd/tree/v1.20261113.1), chiefly
  `src/workerd/server/workerd.capnp` (the config schema), `src/workerd/api/export-loopback.h`,
  `src/workerd/server/server/in_process.rs`, `README.md` and `build/deps/v8.MODULE.bazel`.
- The throwaway measurement code is on the branch **`spike/3629-workerd-measure`** (commit
  `2a30c8604164bae9ced88ba577243d3d0f5e62b9`), under `spike/workerd/`. It is never to be
  merged. It holds:
  - `handler.js`: #3628's `fixtures/js-handler/handler.ts` with the types stripped. The one
    change of substance is that every kernel call is awaited, because a workerd binding is an
    RPC stub and RPC is asynchronous. It adds probes for authority the handler was not handed.
  - `kernel.js`: a stand-in kernel as a second workerd service. Its grant arrives in
    `ctx.props`, set on the binding that hands it to the handler; anything outside the grant
    is refused with an error named `Denied`, as the kernel refuses.
  - `gen_config.py`: writes the workerd config: with or without the binding, with the default
    `globalOutbound` or a refusing one, with or without `--jitless`.
  - `measure.py` and its output, `result.txt`.

## 1. The capability model

**How a binding is granted.** Only in the config. A Worker's `bindings` list names each object
it gets in `env`; a `service` binding names another service and, optionally, an `entrypoint` and
`props`. The handler sees nothing else under `env`. Observed: with the binding the handler
reads `urn:host:name` (`hello, world`); with no binding, the same module fails with
`TypeError: Cannot read properties of undefined (reading 'source')`. That is #3628's grant test
shape: the same module, refused, then granted.

**Attenuation.** Partly.
- `props` on a service binding (`ServiceDesignator.props`, `workerd.capnp`) reach the callee as
  `ctx.props`, and the caller cannot change them. Our stand-in kernel used them as the grant:
  `source urn:host:name` and `sink urn:host:log`. `urn:host:secret` was refused
  (`Denied: urn:host:secret`), uncaught and caught alike. So one service can serve differently
  narrowed capabilities to different callers, decided in the config.
- In code, a Worker can mint a stub of one of *its own* entrypoints with chosen props
  (`ctx.exports.<Entrypoint>({props})`, `export-loopback.h`, on by default from compatibility
  date 2025-11-17). That lets the service that owns a resource hand out a narrower stub of
  itself. Read in source, not run.
- A Worker can pass an RPC stub it holds on to another Worker, but it cannot narrow a stub it
  did not mint. To pass on less, it must wrap the stub in an object of its own (a membrane) and
  pass that.

**Compared with `urn:cap:*` grants.** In ikigai, attenuation is a property of the capability:
every hop down a call chain can narrow it, and the kernel checks each request against the
caller's capability. In workerd, attenuation is a property of the *service that issues the
stub*: narrowing is the issuer's code, or the config's `props`, and the runtime does not check
anything on a call beyond "this Worker holds this stub". A grant that attenuates down a chain
of three Workers needs every Worker to be written to do it.

**What code can reach without being handed it (ambient authority).** Observed, unless marked:
- **The public internet, by default.** `globalOutbound` defaults to the implicit `internet`
  service, which allows publicly routable addresses. A handler with *no bindings at all*
  fetched `https://example.com/` (`200`, 577 bytes). Loopback (`127.0.0.1`) was refused, because
  the default allows only `public`. Setting `globalOutbound` to a `network` service that allows
  nothing refused both. This is the one open door that matters, and it is open unless the
  config closes it.
- **Timers.** `setTimeout` works (`timer fired`). It is a clock, not a resource, but it is not
  granted.
- **Globals present without a binding:** `caches`, `WebSocket` and a `process` object whose
  `env` is empty (0 keys). Whether `caches` stores anything with no `cacheApiOutbound` set was
  not tested.
- **The filesystem.** No `disk` service is reachable unless bound. Whether the Node.js
  compatibility layer's `node:fs` exposes anything with `nodejs_compat` on was not checked; we
  did not turn it on.
- **The sandbox itself.** workerd's README says: "`workerd` is not a hardened sandbox", and
  "When using `workerd` to run possibly-malicious code, you must run it inside an appropriate
  secure sandbox, such as a virtual machine." The isolation between Workers is V8's, with none
  of the defense in depth Cloudflare's hosted service adds.

## 2. Embedding

**Inside an ikigai process: not as a supported thing.** workerd ships as a program, `workerd
serve`. Its `main` is now Rust (`src/workerd/server/workerd.rs` calls `workerd_cli::main()`), over
the C++ server and its own build of V8 (15.8.62.5, with 54 local patches under `patches/v8`). There
is an in-process server (`in_process.rs`), but its header says it is "the server run inside a test
process, for `server-test.c++`": it is a test harness, not an embedding API, and it is built only
by Bazel inside the workerd tree. Hosting it inside ikigai would mean vendoring workerd's whole
Bazel build and following its internals daily.

**rusty_v8 with workerd's binding layer: no such seam.** workerd does not use rusty_v8; it
builds V8 from the V8 tag with its own patches, and its binding layer (JSG, `src/workerd/jsg`)
is C++ tied to that build. The item says rusty_v8 is to continue toward workerd after Deno joined
Cloudflare; we did not verify that, and nothing in this tag uses it. Taking rusty_v8 alone would
give us V8 but none of workerd's capability layer: we would be writing our own bindings, which
is #3628's work over again with a different engine.

**Beside it, over a socket: yes.** A workerd `external` service can point at
`unix:/path/to/socket` (`workerd.capnp`, `ExternalServer.address`). So a binding can be a service
whose other end is ikigai: each call becomes an HTTP request over a Unix socket to a kernel
front, which resolves it as a Source or Sink on a name.

**What a binding that IS a kernel request would look like.** One external service per grant,
whose socket the kernel opens for that grant alone, so the socket is the capability:
`env.ikigai.source("urn:host:name")` sends a request on that socket, and the kernel serves it
under the capability bound to that socket, traced and denied like any other request. To keep
R-D2 and R-D6 ("a binding IS a kernel request under the caller's capability, and nothing else
reaches out"), every workerd config we write must also set `globalOutbound` (and
`cacheApiOutbound`) to services that refuse, bind no `disk`, `fromEnvironment` or `unsafeEval`,
and leave `nodejs_compat` off until checked. The guarantee then rests on the config being
right, not on the absence of a route: unlike ikigai's empty WASI context, the default is open.
The caller's capability also cannot follow the call into workerd: workerd sees one socket, not
the invocation that made the request, so the golden thread and per-call attenuation stop at the
socket unless we carry them in every request ourselves.

## 3. Cost

workerd's numbers are from `result.txt` on the spike branch (macOS arm64, the npm binary,
`globalOutbound` refused, the handler bound to the stand-in kernel). #3628's are from
`measurements/js-handler-macos.txt` on main. **They do not measure the same thing**, so read the
notes under the table before comparing.

| Measure | #3628 QuickJS in wasm | workerd, JIT | workerd, `--jitless` |
|---|---|---|---|
| Artifact size | module 909,165 B | binary 150,732,872 B (94,807,256 B stripped) | same |
| Cold: first `urn:js:hello` | 66 ms (engine, compile, first call) | 11.2 ms median of 7 (process spawn to first HTTP reply) | 13.7 ms |
| Warm: one kernel read | 71 µs | 15.9 µs per RPC inside the isolate; 90.4 µs per HTTP call | 16.0 µs; 91.2 µs |
| Warm: read plus write | 78 µs | 34.3 µs inside the isolate; 106.5 µs per HTTP call | 34.3 µs; 107.3 µs |
| Warm: empty call | not measured | 73.3 µs per HTTP call (`urn:js:noop`) | 73.4 µs |
| 1e6-iteration loop | 68 ms | 4.6 ms | 9.7 ms |
| Memory | not measured | 34 MB RSS idle, 37 MB after one call (about 10 MB physical footprint idle) | the same within 0.1 MB |

Notes on what each number is:
- **Warm calls are not alike.** #3628 instantiates a fresh module for every call, in process.
  workerd reuses one isolate across calls, which is how it runs. The "inside the isolate" figure
  is 20,000 kernel calls in a loop, timed with `Date.now()` in the handler: a cross-isolate RPC in
  one process. The "per HTTP call" figure adds a loopback HTTP round trip from a Python client,
  which is most of it: the empty call alone is 73 µs. A binding over a Unix socket to ikigai
  would add a round trip of that kind to every kernel call.
- **Cold start** is the whole process: spawn, config, isolate, compile both small Workers, and
  the first reply. It is low because V8 starts from a snapshot and the handler is a few hundred
  bytes of JavaScript; #3628's cold cost is compiling the 909 KB QuickJS module, which #3628's
  compiled-form cache (R-D7) removes after the first run.
- **Memory**: #3628 recorded no memory figure, so there is nothing to compare against. After
  the full measurement run (about 6,000 HTTP calls and 40,000 in-isolate kernel calls) workerd's
  RSS had grown to 331 MB (JIT) and 322 MB (jitless). We set no heap limit and did not look into
  whether this is uncollected garbage or retained; it needs a look before any use.
- **JIT versus no JIT:** for glue code (a kernel call and a string) `--jitless` changes nothing
  measurable: the cost is the call, not the JavaScript. For the hot loop the JIT is 2.1 times
  faster than V8's interpreter (4.6 ms against 9.7 ms), and V8's interpreter is still 7 times
  faster than QuickJS in wasm (68 ms). `--jitless` costs 2.5 ms of cold start.

## 4. What we would take on

- **License:** Apache-2.0, checked in the tag's `LICENSE` and in the npm package metadata. V8 is
  under its own BSD-style license (not checked in this spike). Apache-2.0 is compatible with this repository's MIT or Apache-2.0.
- **Size:** a 150 MB binary for macOS arm64 (41 MB compressed on npm). It replaces nothing in
  ikigai-runtime; it is in addition to wasmtime.
- **Toolchain:** Bazel 9.3.1 (`.bazelversion`), C++ with Cap'n Proto and KJ, Rust (the entry
  point and parts of the server are now Rust crates), and its own patched V8. On Linux the build
  downloads its own LLVM and sysroot; on macOS it needs Xcode 26.6. We could not build it inside
  Cargo; we would consume the prebuilt binaries.
- **Platforms:** prebuilt for Linux x86-64 and arm64, macOS x86-64 and arm64, and Windows
  x86-64 (npm `optionalDependencies`). No Windows arm64. **No browser**: workerd is a server
  process, so a page cannot run it, while #3628's module runs wherever ikigai-runtime runs,
  including "in a page where a page can run it" (this repository's README).
- **Release cadence:** a release every day; the 30 most recent GitHub releases are
  `v1.20261015.1` through `v1.20261113.1`, one per day. The version is a date, and old
  compatibility dates keep working, so updates are low-risk for JavaScript but frequent for
  anyone tracking security fixes.
- **Who maintains it:** Cloudflare, in `cloudflare/workerd`; the repository was last pushed on
  2026-11-13. Deno's team joining Cloudflare (from the item; not verified here) adds people to
  Cloudflare's runtime work, but does not change who decides what workerd is for: running
  Cloudflare's Workers platform. Features follow that platform, as the cluster and deployment
  machinery in this tag's config schema shows.

## 5. Recommendation

**One path: #3628's, a JavaScript engine compiled to WebAssembly inside ikigai-runtime.** The
trade, named: we give up V8's speed on hot loops (7 to 15 times faster here) and workerd's
Workers APIs, to keep one authority model, in which a guest's only route out is a kernel request
under the caller's capability, the default is no authority, the capability attenuates down a call
chain and the check happens on every call, and the same module runs in the jail, natively and in
a page.

workerd's bindings are the right idea, and closer to ikigai's grants than any other JavaScript
runtime's, but in workerd the capability ends at the config: attenuation is code the issuer
writes, the network is open unless closed, its authors call it not a hardened sandbox, and it
can only sit beside ikigai behind a socket that the caller's capability cannot cross.

**When to look again:** a job whose cost is CPU-bound JavaScript from code we trust, where 68 ms
per million iterations is too slow. Then the cheaper next step is to measure a faster engine
inside the same wasm path before taking on a second runtime. If workerd is still wanted then,
run it beside ikigai as a peer, one external service per grant over a Unix socket, with
`globalOutbound` refused, and treat it as a separate trust domain, not as a host of grants.
