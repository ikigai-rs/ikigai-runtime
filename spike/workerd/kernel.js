// Stand-in for the ikigai kernel, as a workerd service. Its grant arrives in ctx.props, set by
// the config on the binding that hands it to the handler: the binding is the capability, and the
// props attenuate it. A request outside the grant is refused, typed, the way the kernel refuses.
import { WorkerEntrypoint } from "cloudflare:workers";

const store = new Map([["urn:host:name", "world"], ["urn:host:secret", "hunter2"]]);

class Denied extends Error { constructor(iri) { super(iri); this.name = "Denied"; } }

export class Kernel extends WorkerEntrypoint {
  #check(verb, iri) {
    const grant = this.ctx.props?.grant ?? [];
    if (!grant.includes(`${verb} ${iri}`)) throw new Denied(iri);
  }
  source(iri) { this.#check("source", iri); return store.get(iri); }
  sink(iri, text) { this.#check("sink", iri); store.set(iri, text); }
}

export default { fetch() { return new Response("kernel", { status: 404 }); } };
