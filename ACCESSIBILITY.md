# Accessibility

What `ikigai-runtime` and its sibling crates emit that people read, the rules that output follows,
how it is tested, and how to report a barrier. They are libraries that host WebAssembly: what they
write is error text, which a host prints or serves. It is not a conformance claim. Adapted from
the [ACCESSIBILITY.md template](https://github.com/mgifford/ACCESSIBILITY.md) (MIT).

| Field | Value |
|---|---|
| Target | WCAG 2.2 Level AA, for any page that presents this output; this repository writes text only |
| Status | Evaluation in progress. No conformance claim. |
| Owner | the maintainers of this repository |
| Report a barrier | [a GitHub issue](https://github.com/ikigai-rs/ikigai-runtime/issues/new?labels=accessibility) with the `accessibility` label; anything security-sensitive through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-runtime/security/advisories/new) |
| Last reviewed | 2026-11-12 |

## What it emits that people read

- **Run failures** (`crates/ikigai-runtime-wasmtime/src/budget.rs`, `trap_to_error`), as typed
  `ikigai_core::Error`s: a module over its memory, fuel or time budget, past its deadline, trapped,
  or failed; and the refusal of a fuel budget an epoch-only engine cannot enforce.
- **A session module's `proc_exit`** (`crates/ikigai-runtime-wasmtime/src/core_tier.rs`): an
  endpoint error naming the exit code, for example `the module called proc_exit with code 7,
  which ends the call without a reply`.
- **Mount and grant refusals**: grant plan errors (`PlanError`, `crates/ikigai-runtime/src/grants.rs`:
  the grant's scope and why it is refused, for example a path that climbs out of the jail root);
  an artifact whose bytes do not match its digest (`LoadError`, `crates/ikigai-runtime/src/loader.rs`); a module that
  fits no calling convention or has no bound (`EngineError`, `crates/ikigai-runtime/src/engine.rs`); and the component
  tier's conflicts (`space.rs`, `imports.rs` and `project.rs` under
  `crates/ikigai-runtime-wasmtime/src/component/`), such as two exports
  projecting to one name or an import bound twice.
- **Argument errors** for a projected export (`crates/ikigai-runtime-wasmtime/src/component/values.rs`), naming the argument
  and the value, for example `` `twelve` is not a <WIT type> ``.
- **A component's `err` result**, answered as `application/problem+json` around the component's
  own error value: the shape is this repository's, the words are the component's.
- **`ImportBindings`' `Display`**: one plain line per bound import, `interface.function -> verb
  target`.
- **Endpoint descriptions** of projected exports (`crates/ikigai-runtime-wasmtime/src/component/project.rs`): summaries taken
  from the component's own WIT, which are the component author's words.

Not this repository's: what a guest module itself writes (a WASI command's stdout or stderr
becomes its answer as written; a session module's goes only to the host's
`with_session_stdio` sink), and the screen or page a host shows any of this on. Nothing here
writes HTML, SVG, color or a terminal screen.

## Evaluation results

Reviewed on 2026-11-12 against main at
[`13d0ef8556653cde47a89ad979890c386b0c46e2`](https://github.com/ikigai-rs/ikigai-runtime/commit/13d0ef8556653cde47a89ad979890c386b0c46e2).
Pass means only the stated check passed within its stated scope. It does not establish WCAG
2.2 AA conformance or accessibility of a host application. Fail records an observed limitation;
Blocked means the check was not performed. Evaluation is in progress.

The automated command below uses `cargo 1.99.0 (5f94df478 2026-08-27)` and
`rustc 1.99.0 (b940084d7 2026-09-28)` on Linux, with the `wasm32-unknown-unknown`,
`wasm32-wasip1` and `wasm32-wasip2` targets installed:

```sh
cargo test -p ikigai-runtime-wasmtime --features jail-fixtures
```

Paths in the table are relative to `crates/ikigai-runtime-wasmtime/`. The
`jail-fixtures (wasm)` job in [the CI workflow](.github/workflows/ci.yml) repeats this
command on pull requests and main. These assertions check error types, selected names and
message fragments; they do not measure whether people can understand or operate a host.

| Check | Result | Evidence |
|---|---|---|
| Grant refusals identify the path or link | Pass | The command above runs `tests/probe.rs`: `a_grant_path_that_climbs_out_of_the_jail_root_is_refused`, `an_fs_grant_names_a_host_path_and_only_one_inside_the_jail_is_served`, `a_grant_through_a_symbolic_link_is_refused_naming_the_link`. Assertions cover these fixtures, not every grant message. |
| A jail root of `/` is refused with an explanation | Pass | `tests/probe.rs`: `a_jail_root_of_slash_is_refused_at_mount`, run by the command above, asserts the `fs_root` argument name and `whole disk` explanation; it does not assert that the path is printed. |
| Colliding projected exports are both named | Pass | `tests/component.rs`: `two_exports_projecting_to_one_name_refuse_the_mount`, run by the command above, asserts both export names occur in the conflict message. |
| Component error results preserve the component's message and problem media type | Pass | `tests/component.rs`: `options_results_and_enums_follow_the_type_map`, run by the command above, asserts `application/problem+json` and the fixture's `is not a number` text. Guest-authored wording is not evaluated. |
| A kernel denial retains its type and resource name across the jail | Pass | `tests/jail.rs`: `the_capability_floor_holds_across_the_jail`, run by the command above, asserts `Denied` and `urn:host:name` for the fixture. |
| A fuel overrun retains its error type | Pass | `tests/component.rs`: `a_stock_export_spends_its_fuel_budget_as_over_budget`, run by the command above, asserts `OverBudget`; this does not check the wording. |
| Run failures identify the module, artifact or endpoint and explain how to raise a bound | Fail | Source inspection on 2026-11-12: `src/budget.rs`, `trap_to_error`, emits generic `module` messages without those identifiers or guidance. |
| Run failure wording avoids implementation details | Fail | Source inspection on 2026-11-12: `src/budget.rs` formats fuel as `budget Some(…)` with Rust debug formatting and passes the engine error chain through in `module failed: …`. |
| Plain-text names avoid Markdown punctuation | Fail | Source inspection on 2026-11-12: workspace-root path `crates/ikigai-runtime/src/grants.rs`, `check_fs_path`, wraps paths and segments in backticks. How assistive technology speaks them has not been tested. |
| All output preserves failure types, names refused inputs and emits no color, escape sequences or markup | Blocked | Historical source review dated 2026-10-31; CI does not repeat that review. The focused tests above do not establish this for all runtime, engine or guest text. |
| Argument errors, loader and engine refusals, import conflicts, import binding display and WIT endpoint descriptions are understandable | Blocked | No usability evaluation of these output surfaces was performed. Automated fixture assertions do not establish understandable wording. |
| Automated accessibility checker | Blocked | No checker was run. This library emits text and problem JSON; a host page or terminal presentation must be selected before evaluating its rendered output. |
| Browser and keyboard operation, focus, contrast, zoom and reflow | Blocked | No host interface or browser was exercised. This repository supplies no page or terminal screen to evaluate; downstream hosts need their own checks. |
| Screen reader | Blocked | No screen reader was used with a host presenting this output. |
| Magnifier | Blocked | No magnifier was used with a host presenting this output. |
| Speech input | Blocked | No speech input tool was used with a host presenting this output. |
| Switch access | Blocked | No switch access device or software was used with a host presenting this output. |
| Braille display | Blocked | No braille display was used with a host presenting this output. |

## Known limitations

The Fail rows above record source-level wording limitations, not results from users or
assistive technology. Guest stdout, stderr, component error values and WIT summaries can
contain arbitrary guest-authored text. Hosts control rendering, navigation and interaction;
none of those experiences is covered by the automated assertions here.

## Contributing

Read this file before changing any output listed above: a run failure, a refusal, an argument
error. Keep meaning in words and name the input a message is about. In the pull request, give
each accessibility check a `Check | Result | Evidence` row with Pass, Fail or Blocked, its
command or test and tool version, or why it could not run. Keep unrun checks as Blocked,
including assistive technology nobody used. A historical run that CI does not repeat is
Blocked with its date.

## Reporting a barrier

[Open an issue](https://github.com/ikigai-rs/ikigai-runtime/issues/new?labels=accessibility) with
the `accessibility` label (the link sets it when GitHub lets you add labels; otherwise a
maintainer adds it): what you were doing, what happened, and your terminal or browser and any
assistive technology, if you are comfortable sharing them. Anything security-sensitive goes
through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-runtime/security/advisories/new).
**We acknowledge a report within 7 days**, and promise nothing further.

## Change history

| Date | Change |
|---|---|
| 2026-10-31 | First version. |
| 2026-11-12 | Record scoped Pass, Fail and Blocked results, current automated evidence and unperformed assistive technology checks. |
