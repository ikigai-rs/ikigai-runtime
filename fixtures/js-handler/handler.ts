// The spike's TypeScript handler (#3628). Compiled to JavaScript by esbuild and run by the
// `js-plugin` fixture (QuickJS under Javy) as a J2 session module. It has no authority of its
// own: `ikigai` is the only way out, and every call it makes is held to the session's grant.

/** The kernel verbs the plugin gives JavaScript, over `ikigai.host_call`. */
interface Kernel {
  /** Source a resource; returns its representation as text. */
  source(iri: string): string;
  /** Sink `text` to a resource. */
  sink(iri: string, text: string): void;
}

/** A typed refusal from the kernel: `name` is the error class, `message` the resource. */
interface KernelError extends Error {
  name: "Denied" | "NotFound" | "Unresolved" | "Endpoint" | "Timeout" | "Unavailable";
}

declare const ikigai: Kernel;

function greet(): string {
  const name = ikigai.source("urn:host:name");
  const line = `hello, ${name}`;
  ikigai.sink("urn:host:log", line);
  return line;
}

function churn(rounds: number): number {
  let acc = 0;
  for (let i = 0; i < rounds; i++) acc = (acc + i * i) % 1000003;
  return acc;
}

export function handle(target: string): string {
  switch (target) {
    // Source one granted resource and Sink one granted resource.
    case "urn:js:greet":
      return greet();
    // The same read the Rust fixture's `urn:jail:hello` makes, for the comparison.
    case "urn:js:hello":
      return `hello, ${ikigai.source("urn:host:name")}`;
    // Source a resource the grant does not cover: the refusal escapes, typed.
    case "urn:js:secret":
      return ikigai.source("urn:host:secret");
    // The same refusal, caught: JavaScript sees the class and the resource.
    case "urn:js:caught":
      try {
        ikigai.source("urn:host:secret");
        return "read";
      } catch (e) {
        const refusal = e as KernelError;
        return `${refusal.name}: ${refusal.message}`;
      }
    // A hot loop with no kernel call, for the interpreter's cost.
    case "urn:js:loop":
      return String(churn(1_000_000));
    default:
      throw new Error(`no such endpoint: ${target}`);
  }
}
