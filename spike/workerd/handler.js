// fixtures/js-handler/handler.ts (#3628), types stripped, as a workerd module Worker. The one
// change of substance: a binding is a JS RPC stub, and RPC is asynchronous, so every kernel call
// is awaited. `env.ikigai` is the only binding; the target comes in the URL path.
async function greet(ikigai) {
  const name = await ikigai.source("urn:host:name");
  const line = `hello, ${name}`;
  await ikigai.sink("urn:host:log", line);
  return line;
}
function churn(rounds) {
  let acc = 0;
  for (let i = 0; i < rounds; i++) acc = (acc + i * i) % 1000003;
  return acc;
}
async function handle(target, ikigai) {
  switch (target) {
    case "urn:js:greet": return greet(ikigai);
    case "urn:js:hello": return `hello, ${await ikigai.source("urn:host:name")}`;
    case "urn:js:secret": return ikigai.source("urn:host:secret");
    case "urn:js:caught":
      try { await ikigai.source("urn:host:secret"); return "read"; }
      catch (e) { return `${e.name}: ${e.message}`; }
    case "urn:js:loop": return String(churn(1_000_000));
    case "urn:js:noop": return "ok";
    // Ambient-authority probes: what the handler can reach that it was not handed.
    case "urn:probe:fetch":
      try { const r = await fetch(globalThis.__probeUrl); return `fetch reached: ${r.status} ${(await r.text()).length} bytes`; }
      catch (e) { return `fetch refused: ${e.message}`; }
    case "urn:probe:timer":
      return await new Promise((ok) => setTimeout(() => ok("timer fired"), 5));
    case "urn:probe:globals":
      return JSON.stringify({ env: Object.keys(globalThis.__env ?? {}), process: typeof process, processEnv: typeof process === "object" ? Object.keys(process.env ?? {}).length : null,
        caches: typeof caches, WebSocket: typeof WebSocket, connect: typeof globalThis.connect });
    default: throw new Error(`no such endpoint: ${target}`);
  }
}
export default {
  async fetch(request, env) {
    const target = decodeURIComponent(new URL(request.url).pathname.slice(1));
    globalThis.__env = env; globalThis.__probeUrl = new URL(request.url).searchParams.get("u") ?? "https://example.com/";
    // Kernel calls in a loop, timed inside the isolate (Date.now advances across I/O).
    if (target.startsWith("bench:")) {
      const [, what, n] = target.split(":");
      const t0 = Date.now();
      for (let i = 0; i < +n; i++) await handle(`urn:js:${what}`, env.ikigai);
      return new Response(String((Date.now() - t0) / +n));
    }
    try { return new Response(await handle(target, env.ikigai)); }
    catch (e) { return new Response(`${e.name}: ${e.message}`, { status: 500 }); }
  },
};
