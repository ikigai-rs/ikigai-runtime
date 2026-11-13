// The spike's TypeScript handler (#3628). Compiled to JavaScript by esbuild and run by the
// `js-plugin` fixture (QuickJS under Javy) as a J2 session module. It has no authority of its
// own: `ikigai` is the only way out, and every call it makes is held to the session's grant.

/** Named arguments: a string or bytes go inline; the others name a resource or a stored value. */
type Arg = string | Uint8Array | { reference: string } | { content: string };

/** A representation, as the generated surface lifts it. */
interface Representation {
  mediaType: string;
  bytes: Uint8Array;
  readonly text: string;
}

/** A codec for one ABI type, generated with the surface. */
interface Codec {
  encode(value: unknown): Uint8Array;
  decode(bytes: Uint8Array): unknown;
}

/**
 * The kernel verbs, GENERATED from the ABI (`fixtures/js-plugin/src/ikigai.js`): one per verb,
 * each taking a target and named arguments.
 */
interface Kernel {
  source(iri: string, args?: Record<string, Arg>): Representation;
  sink(iri: string, args?: Record<string, Arg>): Representation;
  exists(iri: string, args?: Record<string, Arg>): Representation;
  delete(iri: string, args?: Record<string, Arg>): Representation;
  meta(iri: string, args?: Record<string, Arg>): Representation;
  codec: { AbiModuleCall: Codec; AbiModuleReply: Codec };
}

declare const ikigai: Kernel;

function greet(): string {
  const name = ikigai.source("urn:host:name").text;
  const line = `hello, ${name}`;
  ikigai.sink("urn:host:log", { body: line });
  return line;
}

// Every verb the ABI states, each with a named argument; the kernel stand-in echoes the verb and
// its arguments.
function verbs(): string {
  return [
    ikigai.source("urn:host:echo", { a: "1" }).text,
    ikigai.sink("urn:host:echo", { body: new Uint8Array([104, 105]) }).text,
    ikigai.exists("urn:host:echo", { r: { reference: "urn:host:name" } }).text,
    ikigai.delete("urn:host:echo", { c: { content: "b3:00" } }).text,
    ikigai.meta("urn:host:echo").text,
  ].join("|");
}

// Decode and re-encode every ABI vector (sourced from the host) with the generated codec.
function vectors(): string {
  const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  const unhex = (s: string) => new Uint8Array(s.match(/../g)!.map((x) => parseInt(x, 16)));
  const drift: string[] = [];
  let n = 0;
  for (const line of ikigai.source("urn:host:vectors").text.split("\n")) {
    if (!line || line.startsWith("#")) continue;
    const [name, encoded] = line.split(" ");
    const codec = name.startsWith("ModuleCall.") ? ikigai.codec.AbiModuleCall : ikigai.codec.AbiModuleReply;
    if (hex(codec.encode(codec.decode(unhex(encoded)))) !== encoded) drift.push(name);
    n++;
  }
  return drift.length ? `drift: ${drift.join(", ")}` : `${n} vectors`;
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
      return `hello, ${ikigai.source("urn:host:name").text}`;
    // Source a resource the grant does not cover: the refusal escapes, typed.
    case "urn:js:secret":
      return ikigai.source("urn:host:secret").text;
    // The same refusal, caught: JavaScript sees the class and the resource.
    case "urn:js:caught":
      try {
        ikigai.source("urn:host:secret");
        return "read";
      } catch (e) {
        const refusal = e as KernelError;
        return `${refusal.name}: ${refusal.message}`;
      }
    case "urn:js:verbs":
      return verbs();
    case "urn:js:vectors":
      return vectors();
    // A hot loop with no kernel call, for the interpreter's cost.
    case "urn:js:loop":
      return String(churn(1_000_000));
    default:
      throw new Error(`no such endpoint: ${target}`);
  }
}
