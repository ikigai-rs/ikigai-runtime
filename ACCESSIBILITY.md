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
| Last reviewed | 2026-10-31 |

## What it emits that people read

- **Run failures** (`crates/ikigai-runtime-wasmtime/src/budget.rs`, `trap_to_error`), as typed
  `ikigai_core::Error`s: a module over its memory, fuel or time budget, past its deadline, trapped,
  or failed; and the refusal of a fuel budget an epoch-only engine cannot enforce.
- **Mount and grant refusals**: grant plan errors (`PlanError`, `crates/ikigai-runtime/src/grants.rs`:
  the grant's scope and why it is refused, for example a path that climbs out of the jail root);
  an artifact whose bytes do not match its digest (`LoadError`, `src/loader.rs`); a module that
  fits no calling convention or has no bound (`EngineError`, `src/engine.rs`); and the component
  tier's conflicts (`src/component/space.rs`, `imports.rs`, `project.rs`), such as two exports
  projecting to one name or an import bound twice.
- **Argument errors** for a projected export (`src/component/values.rs`), naming the argument
  and the value, for example `` `twelve` is not a <WIT type> ``.
- **A component's `err` result**, answered as `application/problem+json` around the component's
  own error value: the shape is this repository's, the words are the component's.
- **`ImportBindings`' `Display`**: one plain line per bound import, `interface.function -> verb
  target`.
- **Endpoint descriptions** of projected exports (`src/component/project.rs`): summaries taken
  from the component's own WIT, which are the component author's words.

Not this repository's: what a guest module itself writes (a WASI command's stdout or stderr
becomes its answer as written), and the screen or page a host shows any of this on. Nothing here
writes HTML, SVG, color or a terminal screen.

## The rules that output follows

Verified by reading the source on 2026-10-31:

- A failure keeps its type: a budget overrun is `over budget`, a host deadline is `timeout`, a
  refused import keeps the kernel's own denial, so a host can say what happened in words rather
  than showing a generic error.
- A grant or mount refusal names what it refuses: the grant's scope, the path, the link, the
  export names that collide, the import bound twice.
- No color, escape sequence or markup is written.

## How it is tested

- `crates/ikigai-runtime-wasmtime/tests/probe.rs`:
  `a_grant_path_that_climbs_out_of_the_jail_root_is_refused`,
  `an_fs_grant_names_a_host_path_and_only_one_inside_the_jail_is_served`,
  `a_jail_root_of_slash_is_refused_at_mount` and
  `a_grant_through_a_symbolic_link_is_refused_naming_the_link` pin that a refusal names the path
  or link.
- `tests/component.rs`: `two_exports_projecting_to_one_name_refuse_the_mount` (both names) and
  `options_results_and_enums_follow_the_type_map` (an `err` result answered as
  `application/problem+json` carrying the component's message); several tests pin that budget
  overruns are typed `OverBudget`, though not their wording.
- `tests/jail.rs`: `the_capability_floor_holds_across_the_jail` (the denial names the resource).
- **No automated accessibility checker, browser or assistive technology has been used on this
  output.**

## Known limitations

Found by reading the source on 2026-10-31, not by testing:

- The run failures (`module ran out of fuel`, `module ran past its deadline`, `module exceeded
  its memory budget`, `module trapped: …`) do not name the module, artifact or endpoint, and do
  not say which grant would raise the bound.
- The fuel message prints its budget in Rust's debug form (`budget Some(…)`), and `module
  failed: …` passes the engine's own error chain through as written.
- Some messages wrap names in backticks, Markdown punctuation that a screen reader may read
  aloud in a plain-text face.

## Contributing

Read this file before changing any output listed above: a run failure, a refusal, an argument
error. Keep meaning in words, name the input a message is about, and say in the pull request what
was tested and what was not.

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
